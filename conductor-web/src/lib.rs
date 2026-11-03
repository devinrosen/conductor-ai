pub mod assets;
pub mod attachments;
pub mod config;
pub mod db_events;
pub mod error;
pub mod events;
pub mod notify;
pub mod openapi;
pub mod push;
pub mod routes;
pub mod state;

#[cfg(test)]
pub mod test_helpers;

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
