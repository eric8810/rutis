use std::any::TypeId;
use std::marker::PhantomData;

/// 服务键 = TypeId + 可选限定名(D21:`ServiceKey = TypeKey`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeKey {
    type_id: TypeId,
    qualifier: Option<&'static str>,
}

impl TypeKey {
    /// 类型主键(默认,无限定名)。
    pub fn of<T: ?Sized + 'static>() -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            qualifier: None,
        }
    }

    /// 带限定名的键:同接口多实例(shaku Keyed 模式)。
    pub fn keyed<T: ?Sized + 'static>(qualifier: &'static str) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            qualifier: Some(qualifier),
        }
    }

    /// 诊断描述(不参与分发)。
    pub fn describe(&self) -> String {
        match self.qualifier {
            Some(q) => format!("{}#{q}", self.type_id_debug()),
            None => self.type_id_debug(),
        }
    }

    fn type_id_debug(&self) -> String {
        format!("{:?}", self.type_id)
    }
}

/// 服务键别名(D21 裁决:`ServiceKey` 不另建类型)。
pub type ServiceKey = TypeKey;

/// 类型化限定名常量(开放问题 2 的裁决:`Key<T>` newtype,shaku Keyed 精神)。
///
/// ```
/// use rutis::{Key, TypeKey};
/// const PRIMARY: Key<str> = Key::new("primary");
/// let k: TypeKey = PRIMARY.into();
/// assert_eq!(k, TypeKey::keyed::<str>("primary"));
/// ```
pub struct Key<T: ?Sized + 'static> {
    name: &'static str,
    _marker: PhantomData<fn() -> T>,
}

impl<T: ?Sized + 'static> Key<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _marker: PhantomData,
        }
    }
}

impl<T: ?Sized + 'static> From<Key<T>> for TypeKey {
    fn from(k: Key<T>) -> Self {
        TypeKey::keyed::<T>(k.name)
    }
}

/// isolate 作用域标识:同 label 字符串合并为同一作用域(TS 语义,D21)。
pub(crate) type ScopeId = std::sync::Arc<str>;
