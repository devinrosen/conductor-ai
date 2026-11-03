pub mod config;
pub mod state;
pub mod theme;
pub mod ui;

#[cfg(test)]
mod test_home_init {
    #[ctor::ctor]
    fn init() {
        conductor_core::test_helpers::isolate_conductor_home();
    }

    #[ctor::dtor]
    fn fini() {
        conductor_core::test_helpers::cleanup_isolated_conductor_home();
    }
}
