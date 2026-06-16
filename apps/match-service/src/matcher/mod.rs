//! 撮合器模块

pub mod simple_matcher;

pub use simple_matcher::{
    SharedSimpleMatcher, new_simple_matcher, new_simple_matcher_with_runtime_store,
};
