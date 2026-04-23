// Re-export from runkon-flow so conductor-core and the engine share one error type.
// runkon-flow's EngineError is a superset: it adds a Workflow(String) variant beyond
// the Cancelled and Persistence variants previously defined here.
pub use runkon_flow::engine_error::EngineError;
