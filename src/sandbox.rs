// sandbox.rs — Sandbox 创建与销毁
//
// 这是整个项目最核心的文件。
//
// 一个 sandbox 的创建过程：
//   1. 准备 OverlayFS 目录（upper_dir, work_dir, merged_dir）
//   2. 挂载 OverlayFS（将基础镜像 + 空写入层合并为一个视图）
//   3. 创建 cgroup（限制内存、CPU、进程数）
//   4. 用 unshare 在新 namespace 中启动 executor 进程
//
// 销毁过程（必须反向清理）：
//   1. Kill executor 进程
//   2. 卸载 OverlayFS
//   3. 删除 cgroup
//   4. 删除临时目录
//
// 重要：namespace / cgroup / OverlayFS 都是 Linux 特有的。
// macOS 上提供 mock 实现用于编译和基本测试。

use anyhow::{Context, Result};
use std::time::Instant;
use tracing::info;

use crate::types::{Sandbox, SandboxState};

// ==================== 配置常量 ====================

const BASE_DIR: &str = "/var/lib/mini-sandbox";
const LOWER_DIR: &str = "/var/lib/mini-sandbox/rootfs";
const CGROUP_BASE: &str = "/sys/fs/cgroup/mini-sandbox";

pub struct SandboxConfig {
    pub memory_limit_bytes: u64,
    pub cpu_quota_us: u64,
    pub cpu_period_us: u64,
    pub pids_max: u64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            memory_limit_bytes: 256 * 1024 * 1024,  // 1GB
            cpu_quota_us: 100_000,                  // 100ms
            cpu_period_us: 100_000,                 // 100ms
            pids_max: 50,                           // 最多 50 个进程，防止 fork bomb
        }
    }
