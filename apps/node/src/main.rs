//! chat-node 二进制入口：只负责把退出码翻译给操作系统，逻辑都在 lib（API-010 run_node）。
use std::process::ExitCode;

fn main() -> ExitCode {
    match chat_node::run_from_env() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // 失败信息同样不含密钥材料。
            eprintln!("chat-node: {error}");
            ExitCode::FAILURE
        }
    }
}
