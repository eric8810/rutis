use std::future::Future;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Weak};

use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

use crate::bus::EventBus;
use crate::effect::{Disposer, Effect, EffectRecord};
use crate::error::{default_sink, CordisError, ErrorSink};
use crate::fiber::{
    join_task, spawn_fiber, FiberInner, FiberState, FiberView, Intent, TransitionTask,
};
use crate::key::{ScopeId, TypeKey};
use crate::registry::{Binding, CheckFn, Registry, StoredValue};
use crate::Plugin;

pub(crate) struct Shared {
    pub handle: Handle,
    pub bus: EventBus,
    pub registry: Registry,
    pub error_sink: ErrorSink,
    pub next_plugin_id: AtomicU64,
}

pub(crate) struct CtxInner {
    pub(crate) shared: Arc<Shared>,
    pub(crate) parent: Option<Ctx>,
    pub(crate) fiber: Weak<FiberInner>,
    pub(crate) isolate: Option<(TypeKey, ScopeId)>,
}

/// 上下文 = `Arc<CtxInner>`(所有权模型:Clone 廉价;isolate/plugin 返回共享内核的新 Ctx)。
#[derive(Clone)]
pub struct Ctx(Arc<CtxInner>);

impl Ctx {
    pub(crate) fn new_child(
        shared: Arc<Shared>,
        parent: &Ctx,
        fiber: Weak<FiberInner>,
        isolate: Option<(TypeKey, ScopeId)>,
    ) -> Self {
        Self(Arc::new(CtxInner {
            shared,
            parent: Some(parent.clone()),
            fiber,
            isolate,
        }))
    }

    pub(crate) fn new_root(shared: Arc<Shared>, fiber: Weak<FiberInner>) -> Self {
        Self(Arc::new(CtxInner {
            shared,
            parent: None,
            fiber,
            isolate: None,
        }))
    }

    pub(crate) fn weak_fiber(&self) -> Weak<FiberInner> {
        self.0.fiber.clone()
    }

    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.0.shared
    }

    pub fn handle(&self) -> &Handle {
        &self.0.shared.handle
    }

    pub(crate) fn error_sink(&self) -> ErrorSink {
        self.0.shared.error_sink.clone()
    }

    /// 自动路径:`Handle::try_current()` 失败返回明确错误,绝不隐式建 runtime(D8)。
    pub fn root() -> Result<Ctx, CordisError> {
        let handle = Handle::try_current().map_err(|_| {
            CordisError::PluginFailed(
                "no tokio runtime in scope; construct inside #[tokio::test]/runtime, or use Ctx::root_with(handle)".into(),
            )
        })?;
        Ok(Self::root_with_sink(handle, default_sink()))
    }

    /// 注入构造(优先路径,D8)。
    pub fn root_with(handle: Handle) -> Ctx {
        Self::root_with_sink(handle, default_sink())
    }

    /// 注入构造 + 自定义 ErrorSink。
    pub fn root_with_sink(handle: Handle, sink: ErrorSink) -> Ctx {
        let shared = Arc::new(Shared {
            handle,
            bus: EventBus::new(),
            registry: Registry::new(),
            error_sink: sink,
            next_plugin_id: AtomicU64::new(1),
        });
        let root_fiber = spawn_fiber(&shared, None, None, true);
        root_fiber.ctx.clone()
    }

    /// root fiber 句柄(root dispose 清子树 / root restart,§五 root_restart)。
    pub fn root_view(&self) -> FiberView {
        let mut current = self.clone();
        while let Some(parent) = current.0.parent.clone() {
            current = parent;
        }
        let fiber = current.0.fiber.upgrade().expect("root fiber alive");
        FiberView::from_inner(fiber)
    }

    /// 事件总线(全局唯一;事件分发不跨 isolate 过滤,D29)。
    pub fn events(&self) -> &EventBus {
        &self.0.shared.bus
    }

    /// scope 解析:沿 Ctx 父链回溯,取该键最近的 isolate 覆盖(§四:保留父链查找)。
    pub(crate) fn scope_for(&self, key: &TypeKey) -> Option<ScopeId> {
        let mut current = Some(self.clone());
        while let Some(ctx) = current {
            if let Some((k, scope)) = &ctx.0.isolate {
                if k == key {
                    return Some(scope.clone());
                }
            }
            current = ctx.0.parent.clone();
        }
        None
    }

    /// isolate 作用域(支柱 3):按 ServiceKey 隔离,同 label 合并(TS 语义,D21);
    /// 返回的 Ctx 保留原 fiber 所有权(D28)。
    pub fn isolate(&self, key: impl Into<TypeKey>, label: &str) -> Ctx {
        Ctx::new_child(
            self.0.shared.clone(),
            self,
            self.0.fiber.clone(),
            Some((key.into(), Arc::from(label))),
        )
    }

    /// 类型键读取(显式定位器,D13):沿父链解析作用域;
    /// provider 非 Active 时不可见,但其子树内自访问除外(清理期自访问,§四)。
    /// 访问方自身失活(Unloading/Disposed)时同样不可见——TS inactive context
    /// 语义(reflect.spec 'service inject leak' 的语言无关内核;provider 子树
    /// 内自访问豁免,与清理期自访问同一条规则)。
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.get_as::<T>(TypeKey::of::<T>())
    }

    /// 带显式 key 的读取(多实例,shaku Keyed 模式)。
    pub fn get_as<T: ?Sized + Send + Sync + 'static>(
        &self,
        key: impl Into<TypeKey>,
    ) -> Option<Arc<T>> {
        let key = key.into();
        let scope = self.scope_for(&key);
        let binding = self.0.shared.registry.lookup(key, scope.as_ref())?;
        let provider = binding.provider.upgrade()?;
        let self_access = self.in_subtree_of(&provider);
        if !self_access {
            match self.0.fiber.upgrade() {
                None => return None,
                Some(accessor)
                    if matches!(
                        accessor.state(),
                        FiberState::Unloading | FiberState::Disposed
                    ) =>
                {
                    return None;
                }
                _ => {}
            }
            let visible = provider.state() == FiberState::Active
                && !binding.removing.load(std::sync::atomic::Ordering::SeqCst);
            if !visible {
                return None;
            }
        }
        binding.value.downcast::<T>()
    }

    fn in_subtree_of(&self, other: &Arc<FiberInner>) -> bool {
        let mut current = self.0.fiber.upgrade();
        while let Some(fiber) = current {
            if Arc::ptr_eq(&fiber, other) {
                return true;
            }
            current = fiber.parent_fiber.as_ref().and_then(|w| w.upgrade());
        }
        false
    }

    /// 值语义注册便捷入口(D13)。
    pub fn provide<T: Send + Sync + 'static>(&self, value: T) -> Result<Disposer, CordisError> {
        self.provide_as::<T>(TypeKey::of::<T>(), Arc::new(value))
    }

    /// trait 对象 / 共享实例注册入口。
    pub fn provide_as<T: ?Sized + Send + Sync + 'static>(
        &self,
        key: impl Into<TypeKey>,
        value: Arc<T>,
    ) -> Result<Disposer, CordisError> {
        self.provide_inner(key.into(), value, None)
    }

    /// 带 `check()` 谓词的注册(§四:check 门控保留)。
    pub fn provide_as_with_check<T: ?Sized + Send + Sync + 'static>(
        &self,
        key: impl Into<TypeKey>,
        value: Arc<T>,
        check: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Disposer, CordisError> {
        self.provide_inner(key.into(), value, Some(Arc::new(check)))
    }

    fn provide_inner<T: ?Sized + Send + Sync + 'static>(
        &self,
        key: TypeKey,
        value: Arc<T>,
        check: Option<CheckFn>,
    ) -> Result<Disposer, CordisError> {
        let fiber = self.0.fiber.upgrade().ok_or(CordisError::InactiveEffect)?;
        {
            // 重入报错检查先于重复注册检查(TS assertActive 语义,fiber.ts:434-436)
            let tr = fiber.transition.lock().unwrap();
            if matches!(tr.state, FiberState::Unloading | FiberState::Disposed) {
                return Err(CordisError::InactiveEffect);
            }
        }
        let scope = self.scope_for(&key);
        let provider_gen = {
            let tr = fiber.transition.lock().unwrap();
            tr.generation
        };
        // 同步原子插入为主操作(评审 #9):重复注册直接把错误返给调用方,
        // 不再丢进 error sink 后假报 Ok
        let stored = self.0.shared.registry.insert_binding(
            key,
            scope.clone(),
            Binding {
                value: StoredValue::new(value),
                provider: Arc::downgrade(&fiber),
                provider_id: fiber.id,
                provider_gen,
                check,
                removing: std::sync::atomic::AtomicBool::new(false),
            },
        )?;
        fiber.provided.lock().unwrap().push((key, scope.clone()));
        if fiber.state() == FiberState::Active {
            self.0.shared.registry.notify_key_changed(key);
        }

        // 清理 effect 只负责删除:驱逐该三元组精确匹配的消费者并最终摘除
        let shared = self.0.shared.clone();
        let pid = fiber.id;
        let evict_scope = scope.clone();
        match self.effect(move || {
            Effect::AsyncDisposer(Box::new(move || {
                let shared = shared.clone();
                let scope = evict_scope.clone();
                Box::pin(
                    async move { evict_and_finalize(&shared, pid, provider_gen, key, scope).await },
                )
            }))
        }) {
            Ok(disposer) => Ok(disposer),
            Err(e) => {
                // 极窄竞态兜底:插入后 fiber 进入卸载——回滚自己的那份插入
                self.0
                    .shared
                    .registry
                    .finalize_binding_if(key, scope, &stored);
                Err(e)
            }
        }
    }

    /// 注册清理效应(D23):`f` 立即执行,返回的清理在卸载时 LIFO 执行。
    /// fiber 已 Disposed/Unloading 时返回 `InactiveEffect`(§四:重入报错)。
    pub fn effect(&self, f: impl FnOnce() -> Effect) -> Result<Disposer, CordisError> {
        let fiber = self.0.fiber.upgrade().ok_or(CordisError::InactiveEffect)?;
        // factory 锁外执行;但"状态检查 + effects 入队"必须在同一临界区
        //(transition → effects 嵌套,锁序无反向):否则检查通过后驱动恰好
        // 卸载取走 effects,新记录漏掉本轮清理而泄漏(评审 P1)
        let record = EffectRecord::new(f());
        let handle = self.handle().clone();
        {
            let tr = fiber.transition.lock().unwrap();
            if matches!(tr.state, FiberState::Unloading | FiberState::Disposed) {
                drop(tr);
                // 生命周期已越过登记点:f() 可能已有副作用(如插入了监听器),
                // 立即排干该记录的清理并返失败
                let sink = self.error_sink();
                let record = record.clone();
                let drain_handle = handle.clone();
                handle.spawn(async move {
                    if let Err(e) = record.drain(&drain_handle).await {
                        sink(e);
                    }
                });
                return Err(CordisError::InactiveEffect);
            }
            fiber.effects.lock().unwrap().push(record.clone());
        }
        Ok(Disposer::new(Box::new(move || {
            let record = record.clone();
            let handle = handle.clone();
            Box::pin(async move { record.drain(&handle).await })
        })))
    }

    /// 装载插件(支柱 1)。返回 FiberView;级联卸载:child dispose 注册为
    /// parent fiber 的 effect(D28:child plugin 自动归 parent fiber 所有)。
    pub fn plugin(&self, p: impl Plugin) -> FiberView {
        let fiber = spawn_fiber(&self.0.shared, Some(self), Some(Arc::new(p)), false);
        let view = FiberView::from_inner(fiber.clone());
        let child = view.clone();
        let sink = self.error_sink();
        let registered = self.effect(move || {
            Effect::AsyncDisposer(Box::new(move || {
                let child = child.clone();
                let sink = sink.clone();
                Box::pin(async move {
                    if let Err(e) = child.dispose().await {
                        sink(e);
                    }
                    Ok(())
                })
            }))
        });
        if registered.is_err() {
            // parent 已失活:处置子 fiber,且不再触发装载(评审 #10:
            // 避免 Dispose 之后入队的重载意图无人处理)
            fiber.post(Intent::Dispose);
        } else {
            fiber.post(Intent::RefreshDeps);
        }
        view
    }

    /// 当前 fiber 代的取消 token(D27:每代独立 token,卸载第②步取消)。
    pub fn cancellation_token(&self) -> CancellationToken {
        match self.0.fiber.upgrade() {
            Some(fiber) => fiber.current_token(),
            // fiber 已析构 ≡ 代已结束:返回预取消 token,cancelled() 不永等(评审 P2)
            None => {
                let token = CancellationToken::new();
                token.cancel();
                token
            }
        }
    }

    /// 等待当前 fiber 代被取消(协作取消;不观察则 dispose 无限等待,D27 限制)。
    pub fn cancelled(&self) -> impl Future<Output = ()> + Send + 'static {
        let token = self.cancellation_token();
        async move { token.cancelled().await }
    }

    /// 触发依赖重查(check() 谓词结果变更等场景)。
    pub fn refresh(&self) {
        self.0.shared.registry.refresh_all();
    }
}

/// 服务摘除(D14):①捕获本次绑定并标记摘除(严格解析立即失败,绑定保留供
/// 清理期自访问)→ ②预取消 + 可 join 的依赖重查(驱动已退出的消费者任务即刻
/// 完成,不 join 永等,评审 #3)→ ③并发排干后按 Arc 身份最终摘除——
/// 摘除窗口内被新 provide 替换过的槽位不动(TS dispose 同步释放槽位语义,
/// 对拍 fiber.spec inertia lock 2)。
async fn evict_and_finalize(
    shared: &Arc<Shared>,
    pid: crate::PluginId,
    provider_gen: u64,
    key: TypeKey,
    scope: Option<crate::key::ScopeId>,
) -> Result<(), CordisError> {
    let old = shared
        .registry
        .lookup(key, scope.as_ref())
        .filter(|b| b.provider_id == pid && b.provider_gen == provider_gen);
    if let Some(binding) = &old {
        binding
            .removing
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    let consumers: Vec<Arc<FiberInner>> = shared
        .registry
        .consumers_of(key, (pid, provider_gen, key, scope.clone()));
    let mut tasks = Vec::new();
    for fiber in &consumers {
        fiber.cancel_current();
        let task = TransitionTask::new();
        // post_join 返回 false 时任务已在 post 内即刻完成(评审 #3)
        fiber.post_join(task.clone(), Intent::RefreshDepsJoin);
        tasks.push(task);
    }
    // 其它注入该键的 fiber(Pending 者)也重查(不取消:可能是等值合并)
    shared.registry.notify_key_changed(key);
    for task in tasks {
        let _ = join_task(&task).await;
    }
    // 清理期自访问结束,最终摘除(仅当槽位未被替换)
    if let Some(binding) = old {
        shared.registry.finalize_binding_if(key, scope, &binding);
    }
    Ok(())
}
