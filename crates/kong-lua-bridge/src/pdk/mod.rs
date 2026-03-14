//! Kong PDK compatibility layer. — Kong PDK 兼容层。

mod kong;
mod ngx;

pub use kong::{inject_kong_pdk, sync_ctx_from_lua};
pub use ngx::{inject_ngx_compat, read_body_filter_args, set_body_filter_args};
