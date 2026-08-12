//! 通用本地进程沙盒:与具体 Agent 解耦的进程级隔离 / 资源管理基础设施。
//! coding Agent 是第一个使用方(每会话一个沙盒),后续 RPA / 其他执行环境可复用同一套。
//!
//! 能力边界(如实):
//! - 进程树必杀干净:Windows 用 Job Object(`KILL_ON_JOB_CLOSE`,句柄全关即整树杀净,应用崩溃
//!   也不留孤儿进程);macOS / Linux 用独立进程组(`killpg`)。
//! - 内存上限(可选):Windows 由 Job Object 强制(`JOB_OBJECT_LIMIT_JOB_MEMORY`,超限即分配失败
//!   / 进程被杀);unix 尽力而为(spawn 前 `setrlimit(RLIMIT_AS)`,只约束该进程及其子进程地址空间)。
//! - 存储台账:每个沙盒可登记一个 `storage_dir`,manager 统计其占用;**删除存储是高危操作,
//!   本模块只统计、绝不主动删目录**。
//! - 文件访问约束不在本层:coding 侧仍靠 `resolve_in_workspace` 路径约束 + cwd,不做文件系统 /
//!   网络级隔离。
//!
//! 两层用法:
//! - `LocalSandbox`:单个沙盒(Job / 进程组句柄 + 选项),`terminate` 幂等整树杀,`stats` 读会计。
//! - `SandboxManager`:id → 沙盒台账(创建时间 / 选项 / 存储目录),供 AppState 全局持有,
//!   退出时 `terminate_all` 一网打尽。
//!
//! 关键设计:`run_command` 这类一次性命令由调用方另建「命令专用」临时沙盒(超时 terminate 只杀
//! 该命令的树,不误杀 manager 里会话沙盒常驻的 dev server);manager 管的会话级沙盒只挂常驻进程。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 沙盒创建选项。全 None / Default = 不限制(等价 `LocalSandbox::new()`)。
#[derive(Debug, Clone, Default)]
pub struct SandboxOptions {
    /// 内存上限(字节);None = 不限。Windows 强制、unix 尽力(见模块头注释)。
    pub memory_limit_bytes: Option<u64>,
    /// 出站网络带宽上限(字节/秒);None = 不限。
    /// Windows 10+ 由 Job Object 原生限速(无需管理员);unix 无等价物(pf/nftables 需 root),忽略并告警。
    pub net_max_bandwidth_bytes_per_sec: Option<u64>,
    /// CPU 上限(1-100 的百分比);None = 不限。
    /// Windows 用 Job CPU rate control 硬上限(HARD_CAP,按处理器时间的百分比);unix 无等价物,忽略并告警。
    pub cpu_limit_percent: Option<u32>,
    /// 同时存活进程数上限;None = 不限。
    /// Windows 用 JOB_OBJECT_LIMIT_ACTIVE_PROCESS(超限后新进程创建失败);unix 无等价物,忽略并告警。
    pub max_processes: Option<u32>,
    /// 磁盘 IO 带宽上限(字节/秒);None = 不限。
    /// Windows 用 Job IO rate control(MaxBandwidth,全局卷生效);unix 无等价物,忽略并告警。
    pub io_max_bandwidth_bytes_per_sec: Option<u64>,
    /// 环境变量白名单(精确名,大小写不敏感):命中疑似密钥特征的变量默认剔除,白名单豁免。
    pub env_keep: Vec<String>,
    /// 该沙盒关联的存储目录(如 coding 会话工作区),仅用于占用统计与展示。
    pub storage_dir: Option<PathBuf>,
}

/// 沙盒资源会计信息(尽力而为:进程退出后仍可从 Job 会计读到累计 / 峰值)。
#[derive(Debug, Clone, Copy, Default)]
pub struct SandboxStats {
    /// 累计 CPU 时间(秒,user + kernel)。
    pub cpu_secs: f64,
    /// 峰值内存(字节;Windows 取 Job 峰值,unix 取当前存活进程 RSS 合计)。
    pub peak_mem_bytes: u64,
    /// 当前存活进程数。
    pub active_processes: u32,
    /// 内存上限(字节,展示用;None = 不限)。
    pub mem_limit_bytes: Option<u64>,
    /// 出站带宽上限(字节/秒,展示用;None = 不限)。
    pub net_limit_bytes_per_sec: Option<u64>,
    /// CPU 上限(百分比,展示用;None = 不限)。
    pub cpu_limit_percent: Option<u32>,
    /// 进程数上限(展示用;None = 不限)。
    pub max_processes: Option<u32>,
    /// 磁盘 IO 带宽上限(字节/秒,展示用;None = 不限)。
    pub io_limit_bytes_per_sec: Option<u64>,
}

/// 本地进程沙盒句柄。Drop 时关闭底层句柄(Windows 下配合 KILL_ON_JOB_CLOSE 兜底杀净残留)。
pub struct LocalSandbox {
    imp: Imp,
    limits: Limits,
    env_keep: Vec<String>,
    /// 最近活动时间(get_or_create / assign_pid 刷新),供空闲回收判定;与 manager 台账条目共享。
    last_activity: Arc<Mutex<std::time::SystemTime>>,
}

/// 各限制项的集中存放(stats 回显用;平台强制执行在 Imp 创建时已完成)。
#[derive(Debug, Clone, Copy, Default)]
struct Limits {
    memory_limit_bytes: Option<u64>,
    net_limit_bytes_per_sec: Option<u64>,
    cpu_limit_percent: Option<u32>,
    max_processes: Option<u32>,
    io_limit_bytes_per_sec: Option<u64>,
}

impl LocalSandbox {
    /// 创建无限制沙盒(微秒级,无镜像 / 容器等外部依赖)。
    pub fn new() -> io::Result<Self> {
        Self::with_options(SandboxOptions::default())
    }

    /// 按选项创建沙盒(内存 / CPU / 进程数 / 网络 / IO 限制,环境净化白名单,存储目录台账)。
    pub fn with_options(opts: SandboxOptions) -> io::Result<Self> {
        Ok(Self {
            imp: Imp::new(&opts)?,
            limits: Limits {
                memory_limit_bytes: opts.memory_limit_bytes,
                net_limit_bytes_per_sec: opts.net_max_bandwidth_bytes_per_sec,
                cpu_limit_percent: opts.cpu_limit_percent,
                max_processes: opts.max_processes,
                io_limit_bytes_per_sec: opts.io_max_bandwidth_bytes_per_sec,
            },
            env_keep: opts.env_keep,
            last_activity: Arc::new(Mutex::new(std::time::SystemTime::now())),
        })
    }

    /// spawn 前对命令做平台适配:先做环境变量净化(共享层),再 unix 自立进程组 + 可选 RLIMIT_AS。
    pub(crate) fn configure_command(&self, cmd: &mut tokio::process::Command) {
        // 净化:剔除疑似密钥变量(KEY/SECRET/TOKEN/PASSWORD/CREDENTIAL/AUTH),白名单豁免
        cmd.env_clear();
        for (k, v) in scrubbed_env(&self.env_keep) {
            cmd.env(k, v);
        }
        self.imp.configure_command(cmd);
    }

    /// 把刚 spawn 的子进程纳入沙盒。best-effort 之外的错误由调用方决定是否上抛。
    /// Windows 下存在毫秒级竞态(子进程在 assign 前已退出 / 已产孙进程),可接受:
    /// 入 Job 后其子孙自动归属 Job;竞态窗口内的逃逸由 terminate 时的 Job 语义兜底不到,概率极低。
    pub fn assign_pid(&self, pid: u32) -> io::Result<()> {
        self.touch_activity();
        self.imp.assign_pid(pid)
    }

    /// 刷新最近活动时间(空闲回收判定的输入)。
    fn touch_activity(&self) {
        let mut g = self.last_activity.lock().unwrap_or_else(|e| e.into_inner());
        *g = std::time::SystemTime::now();
    }

    /// 活动时间句柄(manager 台账条目共享同一份)。
    pub(crate) fn activity_handle(&self) -> Arc<Mutex<std::time::SystemTime>> {
        self.last_activity.clone()
    }

    /// 整树终止沙盒内全部进程(幂等:空沙盒 / 重复调用均安全)。
    pub fn terminate(&self) {
        self.imp.terminate();
    }

    /// 读取资源会计信息(失败返回全零,不报错:stats 是展示用,绝不影响主流程)。
    pub fn stats(&self) -> SandboxStats {
        let mut s = self.imp.stats();
        s.mem_limit_bytes = self.limits.memory_limit_bytes;
        s.net_limit_bytes_per_sec = self.limits.net_limit_bytes_per_sec;
        s.cpu_limit_percent = self.limits.cpu_limit_percent;
        s.max_processes = self.limits.max_processes;
        s.io_limit_bytes_per_sec = self.limits.io_limit_bytes_per_sec;
        s
    }
}

// ===================== 环境变量净化(平台共享) =====================

/// 疑似密钥变量的名字特征(大写后子串匹配)。PATH / SystemRoot / HOME 等基础设施变量天然不含这些词。
const SENSITIVE_ENV_MARKERS: &[&str] = &["KEY", "SECRET", "TOKEN", "PASSWORD", "CREDENTIAL", "AUTH"];

/// 过滤环境变量(纯函数,便于单测):剔除名字命中疑似密钥特征的变量,白名单(大写精确名)豁免。
/// 剔除时只记变量名(debug 日志),绝不记值。
pub(crate) fn filter_env<I>(vars: I, keep: &[String]) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (String, String)>,
{
    let keep_up: Vec<String> = keep.iter().map(|k| k.to_uppercase()).collect();
    let mut dropped: Vec<String> = Vec::new();
    let out: Vec<(String, String)> = vars
        .into_iter()
        .filter(|(k, _)| {
            let up = k.to_uppercase();
            let sensitive = SENSITIVE_ENV_MARKERS.iter().any(|m| up.contains(m));
            if sensitive && !keep_up.contains(&up) {
                dropped.push(k.clone());
                false
            } else {
                true
            }
        })
        .collect();
    if !dropped.is_empty() {
        tracing::debug!("沙盒环境变量净化,剔除: {}", dropped.join(", "));
    }
    out
}

/// 以当前进程环境为输入做净化(非 Unicode 名 / 值的变量直接跳过,cmd.env 反正也灌不进)。
fn scrubbed_env(keep: &[String]) -> Vec<(String, String)> {
    filter_env(
        std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))),
        keep,
    )
}

// ===================== Windows:Job Object =====================

#[cfg(windows)]
mod imp_impl {
    use super::{SandboxOptions, SandboxStats};
    use std::io;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, QueryInformationJobObject,
        SetInformationJobObject, SetIoRateControlInformationJobObject, TerminateJobObject,
        JOBOBJECTINFOCLASS, JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION,
        JOBOBJECT_CPU_RATE_CONTROL_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOBOBJECT_IO_RATE_CONTROL_INFORMATION, JOBOBJECT_NET_RATE_CONTROL_INFORMATION,
        JOB_OBJECT_LIMIT, JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOB_OBJECT_LIMIT_CPU_RATE_CONTROL,
        JOB_OBJECT_LIMIT_IO_RATE_CONTROL, JOB_OBJECT_LIMIT_JOB_MEMORY,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_NET_RATE_CONTROL,
        JOB_OBJECT_CPU_RATE_CONTROL_ENABLE, JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
        JOB_OBJECT_IO_RATE_CONTROL_ENABLE, JOB_OBJECT_NET_RATE_CONTROL_ENABLE,
        JOB_OBJECT_NET_RATE_CONTROL_MAX_BANDWIDTH, JobObjectBasicAndIoAccountingInformation,
        JobObjectCpuRateControlInformation, JobObjectExtendedLimitInformation,
        JobObjectNetRateControlInformation,
    };
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

    /// 拥有所有权的句柄包装:Drop 即 CloseHandle(Job 句柄全关时 KILL_ON_JOB_CLOSE 生效)。
    struct OwnedHandle(HANDLE);
    // HANDLE 是 *mut c_void,默认不 Send/Sync;这里仅用于 Win32 调用,跨线程安全
    unsafe impl Send for OwnedHandle {}
    unsafe impl Sync for OwnedHandle {}
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    pub struct Imp {
        job: OwnedHandle,
    }

    impl Imp {
        pub fn new(opts: &SandboxOptions) -> io::Result<Self> {
            let memory_limit_bytes = opts.memory_limit_bytes;
            let net_limit_bytes_per_sec = opts.net_max_bandwidth_bytes_per_sec;
            unsafe {
                let job = CreateJobObjectW(None, windows::core::PCWSTR::null())
                    .map_err(|e| io::Error::other(format!("CreateJobObjectW 失败: {e}")))?;
                // KILL_ON_JOB_CLOSE:最后一个 Job 句柄关闭时杀净树内全部进程——
                // 这是「应用退出 / 崩溃不留沙盒孤儿进程」的兜底。
                let mut flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                if let Some(limit) = memory_limit_bytes {
                    // JOB 级内存上限(整个 Job 所有进程 commit 总和):超限后分配失败 / 进程被杀
                    flags |= JOB_OBJECT_LIMIT_JOB_MEMORY;
                    info.JobMemoryLimit = limit as usize;
                }
                if let Some(max_procs) = opts.max_processes {
                    // 同时存活进程数上限:超限后 Job 内新进程创建失败
                    flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
                    info.BasicLimitInformation.ActiveProcessLimit = max_procs;
                }
                // rate-control 三类 flag(NET/CPU/IO)按 MSDN 需在 LimitFlags 里位或才生效;
                // 但实测这些位超出 JOB_OBJECT_EXTENDED_LIMIT_VALID_FLAGS(0x7FFF),部分 Windows 版本
                // 直接拒绝(E_INVALIDARG)——先试带 flag,被拒则摘掉这三个位重试(降级路径)
                let rate_flags = JOB_OBJECT_LIMIT(
                    JOB_OBJECT_LIMIT_NET_RATE_CONTROL.0
                        | JOB_OBJECT_LIMIT_CPU_RATE_CONTROL.0
                        | JOB_OBJECT_LIMIT_IO_RATE_CONTROL.0,
                );
                let has_rate_limit = net_limit_bytes_per_sec.is_some()
                    || opts.cpu_limit_percent.is_some()
                    || opts.io_max_bandwidth_bytes_per_sec.is_some();
                if has_rate_limit {
                    flags |= rate_flags;
                }
                info.BasicLimitInformation.LimitFlags = flags;
                if let Err(e) = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) {
                    if !has_rate_limit {
                        return Err(io::Error::other(format!("SetInformationJobObject 失败: {e}")));
                    }
                    // 降级:摘掉 NET/CPU/IO rate-control 位重试;rate 信息本身(下面各自的
                    // SetInformationJobObject / SetIoRateControlInformationJobObject)仍可正常设置
                    tracing::warn!(
                        "Job extended limit 带 rate-control 位被拒绝({e}),摘掉后重试(限速仍会设置)"
                    );
                    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT(flags.0 & !rate_flags.0);
                    SetInformationJobObject(
                        job,
                        JobObjectExtendedLimitInformation,
                        &info as *const _ as *const _,
                        size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    )
                    .map_err(|e2| io::Error::other(format!("SetInformationJobObject 失败: {e2}")))?;
                }
                if let Some(net) = net_limit_bytes_per_sec {
                    // 出站带宽限速(Windows 10+ 原生,无需管理员):ENABLE | MAX_BANDWIDTH
                    let net_info = JOBOBJECT_NET_RATE_CONTROL_INFORMATION {
                        MaxBandwidth: net,
                        ControlFlags: JOB_OBJECT_NET_RATE_CONTROL_ENABLE
                            | JOB_OBJECT_NET_RATE_CONTROL_MAX_BANDWIDTH,
                        DscpTag: 0,
                    };
                    SetInformationJobObject(
                        job,
                        JobObjectNetRateControlInformation,
                        &net_info as *const _ as *const _,
                        size_of::<JOBOBJECT_NET_RATE_CONTROL_INFORMATION>() as u32,
                    )
                    .map_err(|e| {
                        io::Error::other(format!("SetInformationJobObject(网络限速) 失败: {e}"))
                    })?;
                }
                if let Some(percent) = opts.cpu_limit_percent {
                    // CPU 硬上限:CpuRate 单位是百分点的百分之一(50% → 5000)
                    let mut cpu_info = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION::default();
                    cpu_info.ControlFlags =
                        JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
                    cpu_info.Anonymous.CpuRate = percent.clamp(1, 100) * 100;
                    SetInformationJobObject(
                        job,
                        JobObjectCpuRateControlInformation,
                        &cpu_info as *const _ as *const _,
                        size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
                    )
                    .map_err(|e| {
                        io::Error::other(format!("SetInformationJobObject(CPU 上限) 失败: {e}"))
                    })?;
                }
                if let Some(io_limit) = opts.io_max_bandwidth_bytes_per_sec {
                    // 磁盘 IO 限速:SDK 无 MAX_BANDWIDTH flag(那是网络限速的),ENABLE + MaxBandwidth 即可;
                    // VolumeName 置空 = 全局生效(专用 API,一次一条,非 SetInformationJobObject)
                    let io_info = JOBOBJECT_IO_RATE_CONTROL_INFORMATION {
                        MaxBandwidth: io_limit as i64,
                        ControlFlags: JOB_OBJECT_IO_RATE_CONTROL_ENABLE.0 as u32,
                        ..Default::default()
                    };
                    let ok = SetIoRateControlInformationJobObject(job, &io_info);
                    if ok == 0 {
                        return Err(io::Error::other(format!(
                            "SetIoRateControlInformationJobObject 失败: {}",
                            windows::core::Error::from_win32()
                        )));
                    }
                }
                Ok(Self { job: OwnedHandle(job) })
            }
        }

        /// Windows 无需预先配置:spawn 后 assign 进 Job 即可(子孙随父自动归属)。
        pub fn configure_command(&self, _cmd: &mut tokio::process::Command) {}

        pub fn assign_pid(&self, pid: u32) -> io::Result<()> {
            unsafe {
                // SET_QUOTA | TERMINATE 是把进程挂进 Job 所需的最小权限集
                let proc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid)
                    .map_err(|e| io::Error::other(format!("OpenProcess({pid}) 失败: {e}")))?;
                let proc = OwnedHandle(proc);
                AssignProcessToJobObject(self.job.0, proc.0)
                    .map_err(|e| io::Error::other(format!("AssignProcessToJobObject({pid}) 失败: {e}")))
            }
        }

        pub fn terminate(&self) {
            unsafe {
                // 1 为退出码;TerminateJobObject 杀净 Job 内全部进程(含孙进程)
                let _ = TerminateJobObject(self.job.0, 1);
            }
        }

        fn query<T: Default>(&self, class: JOBOBJECTINFOCLASS) -> Option<T> {
            let mut info = T::default();
            unsafe {
                QueryInformationJobObject(
                    Some(self.job.0),
                    class,
                    &mut info as *mut _ as *mut _,
                    size_of::<T>() as u32,
                    None,
                )
                .ok()?;
            }
            Some(info)
        }

        pub fn stats(&self) -> SandboxStats {
            let mut s = SandboxStats::default();
            if let Some(acc) = self.query::<JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION>(
                JobObjectBasicAndIoAccountingInformation,
            ) {
                // TotalXxxTime 单位是 100ns
                s.cpu_secs = (acc.BasicInfo.TotalUserTime + acc.BasicInfo.TotalKernelTime) as f64 / 1e7;
                s.active_processes = acc.BasicInfo.ActiveProcesses;
            }
            if let Some(lim) =
                self.query::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>(JobObjectExtendedLimitInformation)
            {
                s.peak_mem_bytes = lim.PeakJobMemoryUsed as u64;
            }
            s
        }
    }
}

// ===================== macOS / Linux:进程组 + setrlimit =====================

#[cfg(unix)]
mod imp_impl {
    use super::{SandboxOptions, SandboxStats};
    use std::io;
    use std::sync::Mutex;

    pub struct Imp {
        /// 本沙盒 spawn 的进程组 id(= 各直接子进程 pid,process_group(0) 使其成为组长)。
        pgids: Mutex<Vec<i32>>,
        /// 内存上限(字节);spawn 前经 pre_exec setrlimit(RLIMIT_AS) 尽力约束。
        memory_limit_bytes: Option<u64>,
    }

    impl Imp {
        pub fn new(opts: &SandboxOptions) -> io::Result<Self> {
            // unix 无原生组级限速 / 限额(pf / nftables / cgroup 需 root 或不可用),以下选项忽略,仅告警;
            // 内存上限仍由 spawn 前 setrlimit(RLIMIT_AS) 尽力约束(见 configure_command)
            if opts.net_max_bandwidth_bytes_per_sec.is_some() {
                tracing::warn!("沙盒网络限速在该平台(unix)不支持,已忽略(仅 Windows 10+ 生效)");
            }
            if opts.cpu_limit_percent.is_some() {
                tracing::warn!("沙盒 CPU 上限在该平台(unix)不支持,已忽略(仅 Windows 生效)");
            }
            if opts.max_processes.is_some() {
                tracing::warn!("沙盒进程数上限在该平台(unix)不支持,已忽略(仅 Windows 生效)");
            }
            if opts.io_max_bandwidth_bytes_per_sec.is_some() {
                tracing::warn!("沙盒磁盘 IO 限速在该平台(unix)不支持,已忽略(仅 Windows 生效)");
            }
            Ok(Self {
                pgids: Mutex::new(Vec::new()),
                memory_limit_bytes: opts.memory_limit_bytes,
            })
        }

        /// 让子进程自立进程组(组 id = 子进程 pid),供 killpg 整组杀;
        /// 有内存上限时在 exec 前 setrlimit(RLIMIT_AS) 约束地址空间(含子进程,rlimit 可继承)。
        pub fn configure_command(&self, cmd: &mut tokio::process::Command) {
            cmd.process_group(0);
            if let Some(limit) = self.memory_limit_bytes {
                use std::os::unix::process::CommandExt;
                // SAFETY:pre_exec 回调跑在 fork 后 exec 前,只能做 async-signal-safe 操作;
                // 这里仅调 setrlimit(薄 syscall 封装,不分配内存、不碰锁),满足约束。
                unsafe {
                    cmd.as_std_mut().pre_exec(move || {
                        let rl = libc::rlimit {
                            rlim_cur: limit as libc::rlim_t,
                            rlim_max: limit as libc::rlim_t,
                        };
                        if libc::setrlimit(libc::RLIMIT_AS, &rl) != 0 {
                            return Err(io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
            }
        }

        pub fn assign_pid(&self, pid: u32) -> io::Result<()> {
            let mut g = self.pgids.lock().unwrap_or_else(|e| e.into_inner());
            g.push(pid as i32);
            Ok(())
        }

        pub fn terminate(&self) {
            let pgids: Vec<i32> = {
                let g = self.pgids.lock().unwrap_or_else(|e| e.into_inner());
                g.clone()
            };
            for pgid in pgids {
                // 组不存在时 killpg 返回 ESRCH,忽略即可(幂等)
                unsafe {
                    libc::killpg(pgid, libc::SIGKILL);
                }
            }
        }

        pub fn stats(&self) -> SandboxStats {
            let pgids: Vec<i32> = {
                let g = self.pgids.lock().unwrap_or_else(|e| e.into_inner());
                g.clone()
            };
            let mut s = SandboxStats::default();
            if pgids.is_empty() {
                return s;
            }
            // 按 pgid 聚合:遍历系统进程,属于本沙盒进程组的计入(getpgid 逐个核对)
            use sysinfo::{ProcessesToUpdate, System};
            let mut sys = System::new();
            sys.refresh_processes(ProcessesToUpdate::All, true);
            for (pid, proc) in sys.processes() {
                let raw = pid.as_u32() as i32;
                let pgid = unsafe { libc::getpgid(raw) };
                if pgid > 0 && pgids.contains(&pgid) {
                    s.active_processes += 1;
                    s.peak_mem_bytes += proc.memory();
                    s.cpu_secs += proc.accumulated_cpu_time() as f64 / 1000.0; // CPU-毫秒 → 秒
                }
            }
            s
        }
    }
}

use imp_impl::Imp;

// ===================== 存储占用统计 =====================

/// 目录占用统计的条目上限:node_modules 这类深目录防爆,封顶即返回已统计部分。
const DIR_SIZE_MAX_ENTRIES: usize = 50_000;

/// 递归求目录占用字节数(同步、带条目上限;读取失败的条目跳过)。
/// 只统计不删除——删除存储是高危操作,刻意不在本模块提供。
pub fn dir_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut entries = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            entries += 1;
            if entries > DIR_SIZE_MAX_ENTRIES {
                return total;
            }
            let Ok(md) = entry.metadata() else { continue };
            if md.is_dir() {
                stack.push(entry.path());
            } else {
                total += md.len();
            }
        }
    }
    total
}

// ===================== 命令审计日志 =====================

/// 沙盒 id 规整为安全文件名(只留字母数字/-/_,防路径穿越)。
fn audit_file_name(sandbox_id: &str) -> String {
    let s: String = sandbox_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if s.is_empty() { "default".to_string() } else { s }
}

/// 审计文件路径:<config_dir>/sandbox-audit/<sandbox_id>.jsonl。
fn audit_path(config_dir: &Path, sandbox_id: &str) -> PathBuf {
    config_dir
        .join("sandbox-audit")
        .join(format!("{}.jsonl", audit_file_name(sandbox_id)))
}

/// 追加一条命令审计记录(JSON 一行):{ts, command, exit_code, duration_ms, timeout}。
/// dev server 这类长驻进程 spawn 时记 exit_code=null。best-effort:写失败只 warn,绝不影响主流程。
pub fn audit(
    config_dir: &Path,
    sandbox_id: &str,
    command: &str,
    exit_code: Option<i32>,
    duration_ms: u128,
    timeout: bool,
) {
    let path = audit_path(config_dir, sandbox_id);
    let line = serde_json::json!({
        "ts": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "command": command,
        "exit_code": exit_code,
        "duration_ms": duration_ms,
        "timeout": timeout,
    });
    let r = (|| -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(f, "{line}")?;
        Ok(())
    })();
    if let Err(e) = r {
        tracing::warn!("写沙盒审计日志失败({}): {e}", path.display());
    }
}

/// 读审计文件尾部 limit 行(原始 JSON 行,前端自行解析);文件不存在返回空。
pub fn read_audit(config_dir: &Path, sandbox_id: &str, limit: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(audit_path(config_dir, sandbox_id)) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(limit);
    lines[start..].iter().map(|s| s.to_string()).collect()
}

// ===================== 存储删除(带护栏,唯一入口) =====================

/// 清空目录内容但保留目录本身;目录不存在视为已清空。
/// 安全护栏(与 commands/mod.rs 的 clear_dir_contents 同语义、略强化):拒绝盘符根及层级过浅
/// 的路径(组件数 < 3,如 `C:\` 或 `C:\foo`),防存储路径误配成根目录时连带清盘。
/// 调用方必须保证 dir 来自 SandboxManager 台账登记的 storage_dir,不接受任意路径。
pub fn clear_storage_dir(dir: &Path) -> io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    if canon.components().count() < 3 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("拒绝清空疑似根目录: {}", canon.display()),
        ));
    }
    for entry in std::fs::read_dir(&canon)? {
        let path = entry?.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

// ===================== SandboxManager(id → 沙盒台账) =====================

/// 一个受管沙盒的台账条目。
pub struct SandboxEntry {
    pub id: String,
    pub sandbox: Arc<LocalSandbox>,
    pub created_at: std::time::SystemTime,
    /// 最近活动时间(与 LocalSandbox 内那份共享;get_or_create / assign_pid 刷新)。
    pub last_activity: Arc<Mutex<std::time::SystemTime>>,
    pub storage_dir: Option<PathBuf>,
    pub memory_limit_bytes: Option<u64>,
}

/// 沙盒台账快照(供 list() 返回 / 上层展示)。
/// 注:id / created_at_secs / storage_dir / memory_limit_bytes 当前消费方(coding 状态面板)只用到
/// stats / storage_bytes,其余字段是给后续「沙盒管理 UI」预留的台账信息。
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SandboxInfo {
    pub id: String,
    /// 创建时间(unix 秒)。
    pub created_at_secs: u64,
    pub storage_dir: Option<PathBuf>,
    pub memory_limit_bytes: Option<u64>,
    pub stats: SandboxStats,
    /// 存储目录占用字节数(无 storage_dir 为 0)。
    pub storage_bytes: u64,
}

/// 全局沙盒管理器:按 id 惰性建 / 查 / 杀沙盒,应用退出时 terminate_all 兜底。
/// coding 会话沙盒(conv_id 作 id)是当前唯一使用方;后续其他 Agent 场景直接复用。
pub struct SandboxManager {
    entries: Mutex<HashMap<String, SandboxEntry>>,
    /// 配置根目录(审计日志落在 <config_dir>/sandbox-audit/)。
    config_dir: PathBuf,
}

impl SandboxManager {
    pub fn new(config_dir: PathBuf) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            config_dir,
        }
    }

    /// 经 manager 写一条审计(等价 free fn audit,省传 config_dir)。
    #[allow(dead_code)] // 当前调用点走 free fn(tools.rs 只有 exec.config_dir),此便捷方法预留
    pub fn audit(&self, sandbox_id: &str, command: &str, exit_code: Option<i32>, duration_ms: u128, timeout: bool) {
        audit(&self.config_dir, sandbox_id, command, exit_code, duration_ms, timeout);
    }

    /// 取或建某 id 的沙盒。已存在直接返回(**忽略本次 opts**:内存上限等选项在建 Job 时固化,
    /// 要换上限须先 remove / terminate_all 让下次重建);命中即刷新活动时间。
    pub fn get_or_create(&self, id: &str, opts: SandboxOptions) -> io::Result<Arc<LocalSandbox>> {
        let mut g = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = g.get(id) {
            e.sandbox.touch_activity();
            return Ok(e.sandbox.clone());
        }
        let sandbox = Arc::new(LocalSandbox::with_options(opts.clone())?);
        g.insert(
            id.to_string(),
            SandboxEntry {
                id: id.to_string(),
                sandbox: sandbox.clone(),
                created_at: std::time::SystemTime::now(),
                last_activity: sandbox.activity_handle(),
                storage_dir: opts.storage_dir,
                memory_limit_bytes: opts.memory_limit_bytes,
            },
        );
        Ok(sandbox)
    }

    /// 按 id 查(不建)。
    // 预留给后续其他使用方 / 管理 UI;coding 目前只走 get_or_create / remove / terminate_all
    #[allow(dead_code)]
    pub fn get(&self, id: &str) -> Option<Arc<LocalSandbox>> {
        let g = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        g.get(id).map(|e| e.sandbox.clone())
    }

    /// 杀某 id 沙盒内全部进程,但保留台账(调用方后续自行 remove 或复用)。
    #[allow(dead_code)] // 同 get:管理面 API,coding 当前用 remove(terminate + 摘台账)
    pub fn terminate(&self, id: &str) {
        let sb = {
            let g = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            g.get(id).map(|e| e.sandbox.clone())
        };
        if let Some(sb) = sb {
            sb.terminate();
        }
    }

    /// 杀进程 + 摘台账(terminate 过的 Job 不能再接收新进程,故要重建须先摘)。
    /// 注意:绝不删除 storage_dir 目录——摘的只是内存台账。
    pub fn remove(&self, id: &str) {
        let entry = {
            let mut g = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            g.remove(id)
        };
        if let Some(e) = entry {
            e.sandbox.terminate();
        }
    }

    /// 全量台账快照(含 stats 与存储占用)。
    pub fn list(&self) -> Vec<SandboxInfo> {
        let entries: Vec<SandboxEntry> = {
            let g = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            g.values()
                .map(|e| SandboxEntry {
                    id: e.id.clone(),
                    sandbox: e.sandbox.clone(),
                    created_at: e.created_at,
                    last_activity: e.last_activity.clone(),
                    storage_dir: e.storage_dir.clone(),
                    memory_limit_bytes: e.memory_limit_bytes,
                })
                .collect()
        };
        entries
            .into_iter()
            .map(|e| SandboxInfo {
                id: e.id,
                created_at_secs: e
                    .created_at
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                storage_bytes: e.storage_dir.as_deref().map(dir_size).unwrap_or(0),
                storage_dir: e.storage_dir,
                memory_limit_bytes: e.memory_limit_bytes,
                stats: e.sandbox.stats(),
            })
            .collect()
    }

    /// 空闲回收:距最近活动超 max_idle 且当前无存活进程的沙盒,terminate + 摘台账(**不动存储**),
    /// 返回被回收的 id 列表。挂着 dev server 的沙盒 active_processes > 0,天然不会被误收;
    /// 回收后下次动作走 get_or_create 惰性重建,语义无缝。
    pub fn recycle_idle(&self, max_idle: std::time::Duration) -> Vec<String> {
        let now = std::time::SystemTime::now();
        let snapshot: Vec<(String, std::time::SystemTime, Arc<LocalSandbox>)> = {
            let g = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            g.values()
                .map(|e| {
                    let last = *e.last_activity.lock().unwrap_or_else(|x| x.into_inner());
                    (e.id.clone(), last, e.sandbox.clone())
                })
                .collect()
        };
        let mut recycled = Vec::new();
        for (id, last, sb) in snapshot {
            let idle_for = now.duration_since(last).unwrap_or_default();
            if idle_for >= max_idle && sb.stats().active_processes == 0 {
                self.remove(&id);
                recycled.push(id);
            }
        }
        if !recycled.is_empty() {
            tracing::info!("回收空闲沙盒: {}", recycled.join(", "));
        }
        recycled
    }

    /// 杀净并清空全部沙盒(停止全部 / 应用退出时调用)。
    /// 即便漏杀,进程退出时 Job 句柄关闭(KILL_ON_JOB_CLOSE)也会兜底。
    pub fn terminate_all(&self) {
        let entries: Vec<SandboxEntry> = {
            let mut g = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            g.drain().map(|(_, e)| e).collect()
        };
        for e in entries {
            e.sandbox.terminate();
        }
    }
}

// ===================== 单测 =====================

#[cfg(test)]
mod tests {
    use super::*;

    /// 存储占用统计:嵌套目录求和正确。
    #[test]
    fn dir_size_sums_nested_files() {
        let root = std::env::temp_dir().join(format!("veltrix-sandbox-test-{}", std::process::id()));
        let sub = root.join("a/b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(root.join("f1.bin"), vec![0u8; 1000]).unwrap();
        std::fs::write(sub.join("f2.bin"), vec![0u8; 2345]).unwrap();
        let size = dir_size(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(size, 3345);
    }

    /// 环境变量净化:疑似密钥剔除、白名单豁免、普通变量保留、大小写不敏感。
    #[test]
    fn filter_env_scrubs_sensitive() {
        let vars = vec![
            ("MY_API_KEY".to_string(), "k".to_string()),
            ("db_password".to_string(), "p".to_string()),
            ("OAUTH_TOKEN".to_string(), "t".to_string()),
            ("NORMAL_VAR".to_string(), "n".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
        ];
        // 白名单豁免 db_password(大小写不敏感)
        let out = filter_env(vars, &["DB_PASSWORD".to_string()]);
        let names: Vec<&str> = out.iter().map(|(k, _)| k.as_str()).collect();
        assert!(!names.contains(&"MY_API_KEY"), "KEY 应被剔除");
        assert!(!names.contains(&"OAUTH_TOKEN"), "TOKEN/AUTH 应被剔除");
        assert!(names.contains(&"db_password"), "白名单应豁免");
        assert!(names.contains(&"NORMAL_VAR") && names.contains(&"PATH"), "普通变量应保留");
    }

    /// 审计:写入后 read_audit 能按尾部 limit 读回;不存在返回空。
    #[test]
    fn audit_write_and_read_tail() {
        let dir = std::env::temp_dir().join(format!("veltrix-audit-test-{}", std::process::id()));
        for i in 0..5 {
            audit(&dir, "conv-1", &format!("cmd-{i}"), Some(0), 10, false);
        }
        audit(&dir, "conv-1", "npm run dev", None, 0, false); // 长驻进程:exit_code=null
        let all = read_audit(&dir, "conv-1", 100);
        assert_eq!(all.len(), 6);
        let tail = read_audit(&dir, "conv-1", 2);
        assert_eq!(tail.len(), 2);
        assert!(tail[0].contains("cmd-4"), "尾部第 1 条应为 cmd-4: {}", tail[0]);
        assert!(tail[1].contains("npm run dev") && tail[1].contains("\"exit_code\":null"));
        assert!(read_audit(&dir, "nonexistent", 10).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 清存储护栏:拒绝根 / 浅层路径;正常目录清空内容但保留目录。
    #[test]
    fn clear_storage_dir_guard_and_clear() {
        let root = if cfg!(windows) { Path::new("C:\\") } else { Path::new("/") };
        assert!(clear_storage_dir(root).is_err(), "根目录必须被拒");
        // 正常目录:清内容、留目录
        let base = std::env::temp_dir()
            .join(format!("veltrix-clear-test-{}", std::process::id()))
            .join("deep")
            .join("storage");
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::write(base.join("f.txt"), b"x").unwrap();
        clear_storage_dir(&base).unwrap();
        assert!(base.exists());
        assert!(std::fs::read_dir(&base).unwrap().next().is_none(), "内容应已清空");
        let _ = std::fs::remove_dir_all(base.parent().unwrap().parent().unwrap());
    }

    /// 空闲回收:超龄且无存活进程的回收(摘台账);新近活动的不动。
    #[test]
    fn recycle_idle_by_last_activity() {
        let m = SandboxManager::new(std::env::temp_dir());
        m.get_or_create("old", SandboxOptions::default()).unwrap();
        m.get_or_create("fresh", SandboxOptions::default()).unwrap();
        // 把 old 的活动时间拨回 2 小时前(不 sleep,直接改共享句柄)
        {
            let g = m.entries.lock().unwrap();
            let mut t = g.get("old").unwrap().last_activity.lock().unwrap();
            *t = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
        }
        let recycled = m.recycle_idle(std::time::Duration::from_secs(1800));
        assert_eq!(recycled, vec!["old".to_string()]);
        assert!(m.get("old").is_none(), "old 应已摘台账");
        assert!(m.get("fresh").is_some(), "fresh 应保留");
    }
}

#[cfg(all(test, windows))]
mod win_tests {
    use super::*;
    use sysinfo::{ProcessesToUpdate, System};

    /// 用 sysinfo 收集 pid 的整棵子孙树(含自身)。
    fn process_tree(root: u32) -> Vec<u32> {
        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let mut tree = vec![root];
        let mut i = 0;
        while i < tree.len() {
            let cur = sysinfo::Pid::from_u32(tree[i]);
            for (pid, proc) in sys.processes() {
                if proc.parent() == Some(cur) {
                    tree.push(pid.as_u32());
                }
            }
            i += 1;
        }
        tree
    }

    fn alive(pid: u32) -> bool {
        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::All, true);
        sys.process(sysinfo::Pid::from_u32(pid)).is_some()
    }

    /// 核心保证:terminate 后整棵进程树(cmd → powershell 孙进程)全部死净。
    #[test]
    fn terminate_kills_whole_tree() {
        let sb = LocalSandbox::new().expect("创建沙盒失败");
        // cmd 套 powershell 长眠,制造「壳 + 孙进程」两层树(模拟 npm → node 的残留场景)
        let child = std::process::Command::new("cmd")
            .args(["/C", "powershell -NoProfile -Command Start-Sleep 300"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn 失败");
        let pid = child.id();
        sb.assign_pid(pid).expect("assign 失败");

        // 等孙进程起来
        std::thread::sleep(std::time::Duration::from_millis(2000));
        let tree = process_tree(pid);
        assert!(tree.len() >= 2, "应至少有 cmd + powershell 两个进程: {tree:?}");

        sb.terminate();
        // 给内核一点时间完成清理
        std::thread::sleep(std::time::Duration::from_millis(500));
        for p in &tree {
            assert!(!alive(*p), "进程 {p} 在 terminate 后仍存活");
        }
    }

    /// stats:跑过进程的 Job 能取到非零会计信息(累计 CPU / 峰值内存)。
    #[test]
    fn stats_after_run() {
        let sb = LocalSandbox::new().expect("创建沙盒失败");
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "echo hello-sandbox"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn 失败");
        sb.assign_pid(child.id()).expect("assign 失败");
        let _ = child.wait();
        // 进程退出后 Job 会计仍保留累计 / 峰值
        let s = sb.stats();
        assert!(s.peak_mem_bytes > 0, "峰值内存应 > 0: {s:?}");
        assert!(s.cpu_secs >= 0.0, "CPU 累计应可读: {s:?}");
        assert_eq!(s.mem_limit_bytes, None, "无限制沙盒 mem_limit 应为 None");
    }

    /// 网络限速:带限速创建沙盒成功 + stats 能读到限速值。
    /// 功能性的带宽实测依赖网络环境,会 flaky,不做;SetInformationJobObject 报错即创建失败,已覆盖。
    #[test]
    fn net_limit_sandbox_stats() {
        const LIMIT: u64 = 256 * 1024;
        let sb = LocalSandbox::with_options(SandboxOptions {
            net_max_bandwidth_bytes_per_sec: Some(LIMIT),
            ..Default::default()
        })
        .expect("创建带限速的沙盒失败");
        assert_eq!(sb.stats().net_limit_bytes_per_sec, Some(LIMIT));
    }

    /// CPU / 进程数 / 磁盘 IO 三限制:创建成功 + stats 回读(功能性压测会 flaky,不做)。
    #[test]
    fn resource_limits_sandbox_stats() {
        let sb = LocalSandbox::with_options(SandboxOptions {
            cpu_limit_percent: Some(50),
            max_processes: Some(8),
            io_max_bandwidth_bytes_per_sec: Some(1024 * 1024),
            ..Default::default()
        })
        .expect("创建带 CPU/进程数/IO 限制的沙盒失败");
        let s = sb.stats();
        assert_eq!(s.cpu_limit_percent, Some(50));
        assert_eq!(s.max_processes, Some(8));
        assert_eq!(s.io_limit_bytes_per_sec, Some(1024 * 1024));
    }

    /// 内存上限:64MB 上限的 Job 里分配 256MB,分配必然失败、进程非正常退出。
    /// 分配量取上限 4 倍留足余量(powershell 自身启动占 ~30-50MB,小于 64MB 可正常拉起)。
    #[test]
    fn memory_limit_kills_hog() {
        const LIMIT: u64 = 64 * 1024 * 1024;
        let sb = LocalSandbox::with_options(SandboxOptions {
            memory_limit_bytes: Some(LIMIT),
            ..Default::default()
        })
        .expect("创建带内存上限的沙盒失败");
        assert_eq!(sb.stats().mem_limit_bytes, Some(LIMIT));
        let child = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                // EAP=Stop:New-Object 分配失败默认只是语句级错误(脚本继续、退出码仍为 0),
                // 必须让它成为脚本级终止,退出码才非 0
                "$ErrorActionPreference='Stop'; $x = New-Object byte[] 256MB; $x[0] = 1; Write-Output allocated",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn 失败");
        sb.assign_pid(child.id()).expect("assign 失败");
        let out = child.wait_with_output().expect("wait 失败");
        let stdout = String::from_utf8_lossy(&out.stdout);
        // Job 内存超限 → OutOfMemoryException(EAP=Stop 下脚本终止,退出码非 0)或进程直接被杀;
        // 若无上限,该命令会打印 allocated 并以 0 退出
        assert!(
            !out.status.success() && !stdout.contains("allocated"),
            "超限分配应失败,实际: status={:?} stdout={stdout}",
            out.status
        );
    }
}
