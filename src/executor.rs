// executor.rs — Service 层的 executor 客户端
//
// 这段代码运行在 sandbox 外部（宿主机上的 Service 进程里）。
// 它通过 OverlayFS 的穿透特性，直接访问 sandbox 内部 executor 创建的 Unix Socket。
//
// 通信路径：
//   Sandbox 内部: executor 监听 /run/executor.sock
//                      ↓ (通过 OverlayFS，socket 文件实际落在 upper_dir)
//   宿主机实际位置: {upper_dir}/run/executor.sock
//                      ↑ (客户端直接连接这个路径)
//   Service 层:   本文件的 execute_in_sandbox() 函数

use anyhow::{Context, Result};
use tokio::net::UnixStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::path::PathBuf;
use std::time::Duration;

use crate::types::{ExecutionRequest, ExecutionResponse};

/// 将代码发送到 sandbox 内的 executor 执行, 返回结果
/// 
/// `upper_dir` 是 OverlayFS 的 upper 目录路径 （宿主机上的真实路径）
/// executor 在sandbox 内部监听 /run/executor.sock 文件
/// 但由于 OverlayF 穿透，这个 socket 实际出现在 {upper_dir}/run/executor.sock
pub async fn execute_in_sandbox(
    upper_dir: &str,
    request: &ExecuteRequest,
) -> Result<ExecuteResponse> {
    // 1. 构造 socket 路径
    let sock_path: PathBuf = [upper_dir, "run", "executor.sock"].iter().collect();

    // 2. 连接 executor (异步 unix socket)
    let mut stream = UnixStream::connect(&sock_path)
        .await
        .with_context(|| format!("failed to connect executor at {:?}", sock_path))?;
       
    // 3. 发送执行请求
    send_request(&mut stream, request).await?;

    // 4. 接收响应
    let response = receive_response(&mut stream).await?;

    // 5. 返回结果
    Ok(response)
}


pub async fn execute_in_sandbox_with_timeout(
    upper_dir: &str,
    request: &ExecuteRequest,
    timeout: Duration,
) -> Result<ExecuteResponse> {
    // 1. 创建一个定时器
    let timeout_handle = tokio::time::timeout(timeout, execute_in_sandbox(upper_dir, request));
    // 2. 等待执行完成或超时
    timeout_handle.await.map_err(|_| anyhow::anyhow!("execution timed out"))?
        .map_err(|e| anyhow::anyhow!("execution failed: {}", e))?
}

// ==================== 长度前缀协议（异步版本） ====================
//
// 和 bin/executor.rs 中的同步版本使用完全相同的协议格式：
//   [4 bytes: u32 big-endian] [JSON body]
//
// 区别是这里用 tokio 的 AsyncRead/AsyncWrite。

async fn send_request(stream: &mut UnixStream, request: &ExecuteRequest) -> Result<()> {
    // 1. 序列化请求
    let body = serde_json::to_vec(request).with_context(|| "failed to serialize request")?;
    let len = body.len() as u32;
    // 2. 发送请求
    stream.write_all(&len.to_be_bytes()).await?;
    // 3. 发送 body
    stream.write_all(&body).await?;
    // 4. 等待发送完成
    stream.flush().await?;

    Ok(())
}

async fn receive_response(stream: &mut UnixStream) -> Result<ExecuteResponse> {
    // 1. 接收长度
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;

    // 安全检查： 防止恶意 executor 发来巨大长度导致 OOM
    if len > 10 * 1024 * 1024 {
        anyhow::bail!("response too large: {} bytes", len);
    }

    // 2. 接收 body
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await?;
    let response = serde_json::from_slice(&body).with_context(|| "failed to deserialize response")?;
    Ok(response)
}