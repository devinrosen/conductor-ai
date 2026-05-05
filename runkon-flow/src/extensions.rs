use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

/// Type-erased extension map for passing executor-specific data through `ActionParams`
/// without polluting the generic API surface.
///
/// Values are stored as `Arc<dyn Any + Send + Sync>` so cloning is cheap (Arc ref-count bump)
/// and the map can cross thread boundaries (required by `parallel.rs`'s per-thread copies).
#[derive(Default, Clone)]
pub struct Extensions {
    map: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl Extensions {
    /// Insert a value of type `T`, replacing any previously inserted value of the same type.
    pub fn insert<T: Any + Send + Sync + 'static>(&mut self, value: T) {
        self.map.insert(TypeId::of::<T>(), Arc::new(value));
    }

    /// Retrieve a cloned `Arc<T>` for a value of type `T`, if one was inserted.
    pub fn get<T: Any + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|arc| arc.clone().downcast::<T>().ok())
    }
}
