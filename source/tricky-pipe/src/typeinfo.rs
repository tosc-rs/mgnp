use core::any;

#[derive(Copy, Clone, Debug)]
pub(crate) struct TypeInfo {
    // These are represented as functions because `core::any::type_name` and
    // `core::any::TypeId::of` are not currently stable as `const fn`s. When
    // these functions are `const fn` on stable, we could just construct the
    // `TypeInfo` with the actual values...
    #[cfg(debug_assertions)]
    id: fn() -> any::TypeId,
    name: fn() -> &'static str,
}

impl TypeInfo {
    pub const fn of<T: 'static>() -> Self {
        Self {
            #[cfg(debug_assertions)]
            id: any::TypeId::of::<T>,
            name: any::type_name::<T>,
        }
    }

    #[cfg(debug_assertions)]
    #[inline]
    #[track_caller]
    pub(crate) fn assert_matches<T: 'static>(self, what: &'static str) {
        assert_eq!(
            (self.id)(),
            any::TypeId::of::<T>(),
            "/!\\ EXTREMELY SERIOUS WARNING: you would have just done a type confusion, this is Real Bad!\n\
            expected: `{what}<{}>`\n   found: `{what}<{}>`",
            any::type_name::<T>(),
            self.name(),
        );
    }

    #[cfg(not(debug_assertions))]
    #[inline]
    pub(crate) fn assert_matches<T: 'static>(self, _: &'static str) {}

    pub(crate) fn name(self) -> &'static str {
        (self.name)()
    }
}
