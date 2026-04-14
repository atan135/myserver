//! proto 模块导出
//!
//! 由 build.rs 自动生成，请勿手动编辑

// myserver.matchservice 是嵌套模块，需要分开声明
pub mod myserver {
    pub mod matchservice {
        include!("myserver.matchservice.rs");
    }
}
