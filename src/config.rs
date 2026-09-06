//! Service configuration.
//!
//! Application-level settings live under
//! `HKLM\SYSTEM\CurrentControlSet\Services\<name>\Parameters`
//! (same layout concept as nssm). TOML import/export is provided for
//! backup and migration.

use serde::{Deserialize, Serialize};
use winreg::enums::*;
use winreg::RegKey;

pub const DEFAULT_ROTATE_BYTES: u32 = 10 * 1024 * 1024; // 10 MiB
pub const DEFAULT_ROTATE_KEEP: u32 = 10; // keep last 10 archived files
pub const DEFAULT_STOP_TIMEOUT: u32 = 1500; // ms per graceful stop level
pub const DEFAULT_RESTART_DELAY: u32 = 1000; // ms before a normal restart
pub const DEFAULT_THROTTLE: u32 = 1500; // ran shorter than this = thrashing
pub const DEFAULT_MAX_RESTARTS: u32 = 10; // give up after N rapid restarts

// Process priority classes (Win32 values).
pub const PRIORITY_IDLE: u32 = 0x00000040;
pub const PRIORITY_BELOW_NORMAL: u32 = 0x00004000;
pub const PRIORITY_NORMAL: u32 = 0x00000020;
pub const PRIORITY_ABOVE_NORMAL: u32 = 0x00008000;
pub const PRIORITY_HIGH: u32 = 0x00000080;
pub const PRIORITY_REALTIME: u32 = 0x00000100;

// Service start types.
pub const START_AUTO: u32 = 2; // SERVICE_AUTO_START
pub const START_DELAYED: u32 = 2; // AUTO_START + DelayedAutoStart flag
pub const START_MANUAL: u32 = 3; // SERVICE_DEMAND_START

fn d_rotate_bytes() -> u32 {
    DEFAULT_ROTATE_BYTES
}
fn d_rotate_keep() -> u32 {
    DEFAULT_ROTATE_KEEP
}
fn d_stop_timeout() -> u32 {
    DEFAULT_STOP_TIMEOUT
}
fn d_restart_delay() -> u32 {
    DEFAULT_RESTART_DELAY
}
fn d_throttle() -> u32 {
    DEFAULT_THROTTLE
}
fn d_max_restarts() -> u32 {
    DEFAULT_MAX_RESTARTS
}
fn d_priority() -> u32 {
    PRIORITY_NORMAL
}
fn d_startup() -> u32 {
    START_AUTO
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    /// Full path of the executable to run (required).
    pub application: String,
    /// Working directory (default: directory of `application`).
    #[serde(default)]
    pub app_directory: String,
    /// Command line parameters passed to the application.
    #[serde(default)]
    pub app_parameters: String,
    /// stdout log file (empty = discard).
    #[serde(default)]
    pub stdout: String,
    /// stderr log file (empty = discard).
    #[serde(default)]
    pub stderr: String,
    /// Rotate log when it grows beyond this many bytes (0 = date-only rotation).
    #[serde(default = "d_rotate_bytes")]
    pub rotate_bytes: u32,
    /// Number of archived log files to keep (0 = keep all).
    #[serde(default = "d_rotate_keep")]
    pub rotate_keep: u32,
    /// Extra environment variables in KEY=VALUE form.
    #[serde(default)]
    pub environment: Vec<String>,
    /// Process priority class (Win32 value, see constants above).
    #[serde(default = "d_priority")]
    pub priority: u32,
    /// 2 = automatic, 3 = manual.
    #[serde(default = "d_startup")]
    pub startup: u32,
    /// Delayed auto-start flag (only meaningful with startup = 2).
    #[serde(default)]
    pub delayed_autostart: bool,
    /// Bitmask of stop methods to skip: 1 console, 2 window, 4 threads, 8 terminate.
    #[serde(default)]
    pub stop_method_skip: u32,
    #[serde(default = "d_stop_timeout")]
    pub stop_timeout_console: u32,
    #[serde(default = "d_stop_timeout")]
    pub stop_timeout_window: u32,
    #[serde(default = "d_stop_timeout")]
    pub stop_timeout_threads: u32,
    /// Delay before restarting a normally-exited application (ms).
    #[serde(default = "d_restart_delay")]
    pub restart_delay_ms: u32,
    /// If the application ran shorter than this, count it as thrashing (ms).
    #[serde(default = "d_throttle")]
    pub throttle_ms: u32,
    /// Give up after this many consecutive rapid restarts (0 = never give up).
    #[serde(default = "d_max_restarts")]
    pub max_restarts: u32,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    /// Services this one depends on.
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Run-as account (empty = LocalSystem). Password is never exported.
    #[serde(default)]
    pub account: String,
    /// Account password - never serialized to TOML.
    #[serde(default, skip_serializing)]
    pub password: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            application: String::new(),
            app_directory: String::new(),
            app_parameters: String::new(),
            stdout: String::new(),
            stderr: String::new(),
            rotate_bytes: DEFAULT_ROTATE_BYTES,
            rotate_keep: DEFAULT_ROTATE_KEEP,
            environment: Vec::new(),
            priority: PRIORITY_NORMAL,
            startup: START_AUTO,
            delayed_autostart: false,
            stop_method_skip: 0,
            stop_timeout_console: DEFAULT_STOP_TIMEOUT,
            stop_timeout_window: DEFAULT_STOP_TIMEOUT,
            stop_timeout_threads: DEFAULT_STOP_TIMEOUT,
            restart_delay_ms: DEFAULT_RESTART_DELAY,
            throttle_ms: DEFAULT_THROTTLE,
            max_restarts: DEFAULT_MAX_RESTARTS,
            display_name: String::new(),
            description: String::new(),
            dependencies: Vec::new(),
            account: String::new(),
            password: String::new(),
        }
    }
}

fn services_key_path(name: &str) -> String {
    format!(r"SYSTEM\CurrentControlSet\Services\{name}")
}

fn parameters_key_path(name: &str) -> String {
    format!(r"SYSTEM\CurrentControlSet\Services\{name}\Parameters")
}

impl Config {
    /// Load the configuration of `name` from the registry.
    pub fn load(name: &str) -> std::io::Result<Config> {
        let hk = RegKey::predef(HKEY_LOCAL_MACHINE);
        let key = hk.open_subkey_with_flags(parameters_key_path(name), KEY_READ)?;
        let s = |k: &str| -> String { key.get_value(k).unwrap_or_default() };
        let u = |k: &str, d: u32| -> u32 { key.get_value::<u32, _>(k).unwrap_or(d) };

        // Service-level values for display/description/account live on the
        // parent key; read them best-effort.
        let svc = hk
            .open_subkey_with_flags(services_key_path(name), KEY_READ)
            .ok();

        Ok(Config {
            application: s("Application"),
            app_directory: s("AppDirectory"),
            app_parameters: s("AppParameters"),
            stdout: s("AppStdout"),
            stderr: s("AppStderr"),
            rotate_bytes: u("AppRotateBytes", DEFAULT_ROTATE_BYTES),
            rotate_keep: u("AppRotateKeep", DEFAULT_ROTATE_KEEP),
            environment: key
                .get_value::<Vec<String>, _>("AppEnvironment")
                .unwrap_or_default(),
            priority: u("AppPriority", PRIORITY_NORMAL),
            startup: svc
                .as_ref()
                .and_then(|k| k.get_value::<u32, _>("Start").ok())
                .unwrap_or(START_AUTO),
            delayed_autostart: svc
                .as_ref()
                .and_then(|k| k.get_value::<u32, _>("DelayedAutoStart").ok())
                .unwrap_or(0)
                != 0,
            stop_method_skip: u("AppStopMethodSkip", 0),
            stop_timeout_console: u("AppStopMethodConsole", DEFAULT_STOP_TIMEOUT),
            stop_timeout_window: u("AppStopMethodWindow", DEFAULT_STOP_TIMEOUT),
            stop_timeout_threads: u("AppStopMethodThreads", DEFAULT_STOP_TIMEOUT),
            restart_delay_ms: u("AppRestartDelay", DEFAULT_RESTART_DELAY),
            throttle_ms: u("AppThrottle", DEFAULT_THROTTLE),
            max_restarts: u("AppMaxRestarts", DEFAULT_MAX_RESTARTS),
            display_name: svc
                .as_ref()
                .and_then(|k| k.get_value::<String, _>("DisplayName").ok())
                .unwrap_or_default(),
            description: svc
                .as_ref()
                .and_then(|k| k.get_value::<String, _>("Description").ok())
                .unwrap_or_default(),
            dependencies: svc
                .as_ref()
                .and_then(|k| k.get_value::<Vec<String>, _>("DependOnService").ok())
                .unwrap_or_default(),
            account: svc
                .as_ref()
                .and_then(|k| k.get_value::<String, _>("ObjectName").ok())
                .unwrap_or_default(),
            password: String::new(),
        })
    }

    /// Write all application-level values into the registry.
    pub fn save_parameters(&self, name: &str) -> std::io::Result<()> {
        let hk = RegKey::predef(HKEY_LOCAL_MACHINE);
        let (key, _) = hk.create_subkey_with_flags(parameters_key_path(name), KEY_WRITE)?;
        key.set_value("Application", &self.application)?;
        key.set_value("AppDirectory", &self.app_directory)?;
        key.set_value("AppParameters", &self.app_parameters)?;
        key.set_value("AppStdout", &self.stdout)?;
        key.set_value("AppStderr", &self.stderr)?;
        key.set_value("AppRotateBytes", &self.rotate_bytes)?;
        key.set_value("AppRotateKeep", &self.rotate_keep)?;
        key.set_value("AppEnvironment", &self.environment)?;
        key.set_value("AppPriority", &self.priority)?;
        key.set_value("AppStopMethodSkip", &self.stop_method_skip)?;
        key.set_value("AppStopMethodConsole", &self.stop_timeout_console)?;
        key.set_value("AppStopMethodWindow", &self.stop_timeout_window)?;
        key.set_value("AppStopMethodThreads", &self.stop_timeout_threads)?;
        key.set_value("AppRestartDelay", &self.restart_delay_ms)?;
        key.set_value("AppThrottle", &self.throttle_ms)?;
        key.set_value("AppMaxRestarts", &self.max_restarts)?;
        Ok(())
    }

    /// Set / clear the DelayedAutoStart flag on the service key.
    pub fn save_delayed_flag(&self, name: &str) -> std::io::Result<()> {
        let hk = RegKey::predef(HKEY_LOCAL_MACHINE);
        let (key, _) = hk.create_subkey_with_flags(services_key_path(name), KEY_WRITE)?;
        if self.delayed_autostart {
            key.set_value("DelayedAutoStart", &1u32)?;
        } else {
            let _ = key.delete_value("DelayedAutoStart");
        }
        Ok(())
    }

    /// Parse `--priority` textual value.
    pub fn parse_priority(s: &str) -> Option<u32> {
        match s.to_ascii_lowercase().replace('_', "-").as_str() {
            "realtime" => Some(PRIORITY_REALTIME),
            "high" => Some(PRIORITY_HIGH),
            "above-normal" | "above" => Some(PRIORITY_ABOVE_NORMAL),
            "normal" => Some(PRIORITY_NORMAL),
            "below-normal" | "below" => Some(PRIORITY_BELOW_NORMAL),
            "idle" => Some(PRIORITY_IDLE),
            _ => None,
        }
    }

    pub fn priority_name(&self) -> &'static str {
        match self.priority {
            PRIORITY_REALTIME => "REALTIME",
            PRIORITY_HIGH => "HIGH",
            PRIORITY_ABOVE_NORMAL => "ABOVE_NORMAL",
            PRIORITY_NORMAL => "NORMAL",
            PRIORITY_BELOW_NORMAL => "BELOW_NORMAL",
            PRIORITY_IDLE => "IDLE",
            _ => "UNKNOWN",
        }
    }

    /// Parse `--startup` textual value; returns (start_type, delayed).
    pub fn parse_startup(s: &str) -> Option<(u32, bool)> {
        match s.to_ascii_lowercase().as_str() {
            "auto" | "automatic" => Some((START_AUTO, false)),
            "delayed" | "delayed-auto" | "delayed-autostart" => Some((START_DELAYED, true)),
            "manual" | "demand" => Some((START_MANUAL, false)),
            _ => None,
        }
    }

    pub fn startup_name(&self) -> &'static str {
        match (self.startup, self.delayed_autostart) {
            (START_AUTO, true) => "自动(延迟启动)",
            (START_AUTO, _) => "自动",
            (START_MANUAL, _) => "手动",
            _ => "未知",
        }
    }
}

/// Pretty-print a config for `rssvc get <name>`.
pub fn print_config(name: &str, c: &Config) {
    println!("服务名称:      {name}");
    if !c.display_name.is_empty() {
        println!("显示名称:      {}", c.display_name);
    }
    if !c.description.is_empty() {
        println!("描述:          {}", c.description);
    }
    println!("启动类型:      {}", c.startup_name());
    if !c.account.is_empty() {
        println!("运行账户:      {}", c.account);
    }
    println!("应用程序:      {}", c.application);
    println!("工作目录:      {}", c.app_directory);
    println!("启动参数:      {}", c.app_parameters);
    if !c.environment.is_empty() {
        println!("环境变量:");
        for e in &c.environment {
            println!("  {e}");
        }
    }
    if !c.stdout.is_empty() {
        println!("stdout 日志:   {}", c.stdout);
    }
    if !c.stderr.is_empty() {
        println!("stderr 日志:   {}", c.stderr);
    }
    println!("进程优先级:    {}", c.priority_name());
    println!("日志轮转:      {} 字节 / 保留 {} 份", c.rotate_bytes, c.rotate_keep);
    println!(
        "停止策略:      跳过掩码 {} (1控制台 2窗口 4线程 8强杀)",
        c.stop_method_skip
    );
    println!(
        "停止超时:      控制台 {}ms / 窗口 {}ms / 线程 {}ms",
        c.stop_timeout_console, c.stop_timeout_window, c.stop_timeout_threads
    );
    println!(
        "重启策略:      延迟 {}ms / 节流阈值 {}ms / 最大连续重启 {}",
        c.restart_delay_ms, c.throttle_ms, c.max_restarts
    );
    if !c.dependencies.is_empty() {
        println!("依赖服务:      {}", c.dependencies.join(", "));
    }
}
