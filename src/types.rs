use serde::{Deserialize, Serialize};
use std::time::Instant;

// ==================== Sandbox 状态 ====================
//
// 一个 sandbox 实例的生命周期：
//   Warming → Idle → Running → Dead
//
// Warming: 正在创建 namespace、挂载文件系统等初始化工作
// Idle:    初始化完成，空闲等待任务分配
// Running: 正在执行用户代码
// Dead:    已失效（崩溃、超时、用完），等待清理

#[derive(Debug, Clone, PartialEq)]
pub enum SandboxState {
    Warming,
    Idle,
    Running,
    Dead,
}

// ==================== 执行请求 ====================
//
// 外部 （比如 AI Agent） 通过 API 发送执行请求
// 用 Deserialize 让 axum 自动解析为这个结构体
#[derive(Debug, Clone, Deserialize)]
pub struct ExecuteRequest {
    pub code: String,
    pub language: String,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub files: Vec<FileInput>,
    #[serde(default)]
    pub env_vars: Vec<(String, String)>,
}

fn default_timeout() -> u64 {
    5_000
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileInput {
    pub path: String,
    #[serde(with = "base64_bytes")]
    pub content: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecuteResponse {
    pub stdout: String, 
    pub stderr: String,
    pub exit_code:i32,
    pub duration_ms: u64,
    pub timed_out: bool,
    pub peak_memory_bytes: u64,
}

// ==================== Sandbox 实例 ====================
//
// 代表一个正在运行的隔离沙箱。
// 不需要序列化——这是纯内部状态。

pub struct Sandbox {
    pub id: String,
    pub state: SandboxState,
    pub pid: u32,
    pub created_at: Instant,
    pub last_used: Instant,
    // 资源路径--销毁时需要依次清理这些
    pub cgroup_path: String, // /sys/fs/cgroup/mini-sandbox/{id}
    pub upper_dir: String, // OverlayFS 的写入层
    pub work_dir: String, // OverlayFS 的工作目录
    pub merged_dir: String, // OverlayFS 的合并挂载点
}

// ==================== 辅助：base64 编码的文件内容 ====================
//
// HTTP JSON 不能直接传二进制，所以文件内容用 base64 编码。
// 这个模块让 serde 自动处理 base64 ↔ Vec<u8> 转换。

mod base64_bytes {
    use serde::{Deserialize, Deserializer, Serializer};
    use serde::de::Error;

    pub fn serialize<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&base64_encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        base64_decode(&s).map_err(D::Error::custom)
    }

    fn base64_encode(input: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut result = String::new();
        for chunk in input.chunks(3) {
            match chunk.len() {
                3 => {
                    let n = (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8 | chunk[2] as u32;
                    result.push(TABLE[(n >> 18 & 0x3F) as usize] as char);
                    result.push(TABLE[(n >> 12 & 0x3F) as usize] as char);
                    result.push(TABLE[(n >> 6 & 0x3F) as usize] as char);
                    result.push(TABLE[(n & 0x3F) as usize] as char);
                }
                2 => {
                    let n = (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8;
                    result.push(TABLE[(n >> 18 & 0x3F) as usize] as char);
                    result.push(TABLE[(n >> 12 & 0x3F) as usize] as char);
                    result.push(TABLE[(n >> 6 & 0x3F) as usize] as char);
                    result.push('=');
                }
                1 => {
                    let n = (chunk[0] as u32) << 16;
                    result.push(TABLE[(n >> 18 & 0x3F) as usize] as char);
                    result.push(TABLE[(n >> 12 & 0x3F) as usize] as char);
                    result.push('=');
                    result.push('=');
                }
                _ => {}
            }
        }
        result
    }

    fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
        // 简易 base64 解码，生产环境应使用 base64 crate
        let input = input.trim();
        if input.is_empty() {
            return Ok(Vec::new());
        }

        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

        let mut buf = Vec::new();
        let bytes: Vec<u8> = input.bytes().filter(|&b| b != b'\n' && b != b'\r').collect();
        let chunks = bytes.chunks(4);

        for chunk in chunks {
            let mut bits: u32 = 0;
            let mut valid = 0;

            for &c in chunk {
                if c == b'=' {
                    break;
                }
                let val = TABLE.iter().position(|&t| t == c)
                    .ok_or_else(|| format!("invalid base64 char: {}", c as char))?;
                bits = (bits << 6) | val as u32;
                valid += 1;
            }

            match valid {
                4 => {
                    buf.push((bits >> 16) as u8);
                    buf.push((bits >> 8) as u8);
                    buf.push(bits as u8);
                }
                3 => {
                    bits <<= 6;
                    buf.push((bits >> 16) as u8);
                    buf.push((bits >> 8) as u8);
                }
                2 => {
                    bits <<= 12;
                    buf.push((bits >> 16) as u8);
                }
                _ => return Err("invalid base64 length".to_string()),
            }
        }

        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_execution_request_basic() {
        let json = r#"{
            "code": "print('hello')",
            "language": "python"
        }"#;
        let req: ExecutionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.code, "print('hello')");
        assert_eq!(req.language, "python");
        assert_eq!(req.timeout_ms, 5000); // 默认值
        assert!(req.files.is_empty());
        assert!(req.env_vars.is_empty());
    }

    #[test]
    fn parse_execution_request_full() {
        let json = r#"{
            "code": "console.log(1)",
            "language": "node",
            "timeout_ms": 3000,
            "env_vars": [["API_KEY", "abc123"]]
        }"#;
        let req: ExecutionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.timeout_ms, 3000);
        assert_eq!(req.env_vars[0], ("API_KEY".to_string(), "abc123".to_string()));
    }

    #[test]
    fn serialize_execution_response() {
        let resp = ExecutionResponse {
            stdout: "2\n".to_string(),
            stderr: String::new(),
            exit_code: 0,
            duration_ms: 45,
            timed_out: false,
            peak_memory_bytes: 1024,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["stdout"], "2\n");
        assert_eq!(json["exit_code"], 0);
        assert_eq!(json["timed_out"], false);
    }

    #[test]
    fn sandbox_state_equality() {
        assert_eq!(SandboxState::Idle, SandboxState::Idle);
        assert_ne!(SandboxState::Idle, SandboxState::Running);
    }
}
