use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};

use crate::error::CordisError;
use crate::fiber::{FiberInner, FiberState, Intent, PluginId};
use crate::key::{ScopeId, TypeKey};

pub(crate) type CheckFn = Arc<dyn Fn() -> bool + Send + Sync>;

/// 类型擦除的服务值:内层 `Box<Arc<T>>` 支持 `T: ?Sized`(trait 对象注册)。
#[derive(Clone)]
pub(crate) struct StoredValue(Arc<Box<dyn Any + Send + Sync>>);

impl StoredValue {
    pub(crate) fn new<T: ?Sized + Send + Sync + 'static>(value: Arc<T>) -> Self {
        Self(Arc::new(Box::new(value) as Box<dyn Any + Send + Sync>))
    }

    pub(crate) fn downcast<T: ?Sized + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        // Box<dyn Any> 擦除的具体类型是内层 Arc<T>(不是 Box<Arc<T>>)
        (**self.0).downcast_ref::<Arc<T>>().cloned()
    }
}

/// 服务绑定。注册表存 `Arc<Binding>`:所有观察者(lookup/依赖解析/摘除)
/// 共享同一身份,`removing` 置位即刻全员可见——不再有克隆快照滞后窗口
///(简化 S1/正确性:旧克隆体的旗标是拍照值,移除中的服务短暂可见)。
pub(crate) struct Binding {
    pub value: StoredValue,
    pub provider: Weak<FiberInner>,
    pub provider_id: PluginId,
    pub provider_gen: u64,
    pub check: Option<CheckFn>,
    /// 摘除已开始(严格解析立即失败;绑定保留至消费者排干,供 provider 子树自访问,§四)。
    pub removing: std::sync::atomic::AtomicBool,
}

/// 服务注册表(支柱 3)+ 反向依赖索引(支柱 5/D21 三元组)。
pub(crate) struct Registry {
    bindings: Mutex<HashMap<(TypeKey, Option<ScopeId>), Arc<Binding>>>,
    /// 注入索引:TypeKey → 声明依赖它的 fiber。
    inject_index: Mutex<HashMap<TypeKey, Vec<Weak<FiberInner>>>>,
}

impl Registry {
    pub(crate) fn new() -> Self {
        Self {
            bindings: Mutex::new(HashMap::new()),
            inject_index: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn insert_binding(
        &self,
        key: TypeKey,
        scope: Option<ScopeId>,
        binding: Binding,
    ) -> Result<Arc<Binding>, CordisError> {
        let mut bindings = self.bindings.lock().unwrap();
        let entry = (key, scope.clone());
        if let Some(existing) = bindings.get(&entry) {
            if !existing.removing.load(std::sync::atomic::Ordering::SeqCst) {
                let scope_desc = entry.1.as_deref().unwrap_or("<default>");
                return Err(CordisError::ServiceExists(format!(
                    "{} in scope {scope_desc}",
                    key.describe()
                )));
            }
            // 摘除进行中的绑定可被同键新 provide 替换(TS dispose 同步释放
            // 注册表槽位;旧绑定由驱逐方按 Arc 身份 finalize,不误伤新绑定)
        }
        let stored = Arc::new(binding);
        bindings.insert(entry, stored.clone());
        Ok(stored)
    }

    pub(crate) fn lookup(&self, key: TypeKey, scope: Option<&ScopeId>) -> Option<Arc<Binding>> {
        let bindings = self.bindings.lock().unwrap();
        bindings.get(&(key, scope.cloned())).cloned()
    }

    /// 标记摘除开始:严格解析立即失败;绑定保留至 [`Registry::finalize_binding`]
    /// (清理期自访问,§四)。
    /// 最终摘除绑定(provider 最后;此前保留供清理期自访问,§四)。
    /// 仅当槽位仍是本次摘除的那份绑定才移除——摘除窗口内被新 provide 替换
    /// 过的槽位不动(TS dispose 同步释放槽位语义,对拍 fiber.spec inertia lock 2)。
    pub(crate) fn finalize_binding_if(
        &self,
        key: TypeKey,
        scope: Option<ScopeId>,
        expected: &Arc<Binding>,
    ) {
        let mut bindings = self.bindings.lock().unwrap();
        let still_old = bindings
            .get(&(key, scope.clone()))
            .is_some_and(|b| Arc::ptr_eq(b, expected));
        if still_old {
            bindings.remove(&(key, scope));
        }
    }

    /// 该依赖四元组的当前消费者(D21 简化:唯一事实源是各 fiber 的
    /// `last_deps`,不再维护第二份反向索引)。声明注入该键、且 `last_deps`
    /// 含此四元组(含作用域)的 fiber 即为绑定中的消费者——与装载/卸载窗口
    /// 严格一致(last_deps 于 load 的 apply 前设置、drain_effects 清除)。
    /// 四元组含作用域:同一 provider fiber 在不同作用域提供的同键绑定
    /// 互不为对方的消费者(isolate 语义)。
    pub(crate) fn consumers_of(
        &self,
        key: TypeKey,
        quad: (PluginId, u64, TypeKey, Option<ScopeId>),
    ) -> Vec<Arc<FiberInner>> {
        let index = self.inject_index.lock().unwrap();
        let Some(list) = index.get(&key) else {
            return Vec::new();
        };
        list.iter()
            .filter_map(|weak| {
                let fiber = weak.upgrade()?;
                let bound = fiber
                    .last_deps
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|deps| deps.contains(&quad));
                bound.then_some(fiber)
            })
            .collect()
    }

    pub(crate) fn register_inject(&self, key: TypeKey, fiber: &Arc<FiberInner>) {
        self.inject_index
            .lock()
            .unwrap()
            .entry(key)
            .or_default()
            .push(Arc::downgrade(fiber));
    }

    /// 通知所有注入该键的 fiber 重查依赖。
    pub(crate) fn notify_key_changed(&self, key: TypeKey) {
        let fibers: Vec<Arc<FiberInner>> = {
            let index = self.inject_index.lock().unwrap();
            index
                .get(&key)
                .map(|list| list.iter().filter_map(|w| w.upgrade()).collect())
                .unwrap_or_default()
        };
        for fiber in fibers {
            fiber.post(Intent::RefreshDeps);
        }
    }

    /// 重查所有声明了依赖的 fiber(`Ctx::refresh`,check() 谓词变更触发)。
    pub(crate) fn refresh_all(&self) {
        let mut seen: HashSet<PluginId> = HashSet::new();
        let index = self.inject_index.lock().unwrap();
        for list in index.values() {
            for weak in list {
                if let Some(fiber) = weak.upgrade() {
                    if seen.insert(fiber.id) {
                        fiber.post(Intent::RefreshDeps);
                    }
                }
            }
        }
    }

    /// 门控解析(支柱 2):存在 + 未在摘除 + provider Active + `check()` 通过。
    pub(crate) fn resolve_dep(
        &self,
        key: TypeKey,
        scope: Option<&ScopeId>,
    ) -> Option<(PluginId, u64)> {
        let binding = self.lookup(key, scope)?;
        if binding.removing.load(std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        let provider = binding.provider.upgrade()?;
        if provider.state() != FiberState::Active {
            return None;
        }
        if let Some(check) = &binding.check {
            // check() 是用户回调:panic 视为不就绪(TS 语义:记日志并删除,
            // fiber.ts:695-698;评审 #6——不得杀调用方所在的驱动任务)
            let passed =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check())).unwrap_or(false);
            if !passed {
                return None;
            }
        }
        Some((binding.provider_id, binding.provider_gen))
    }
}
