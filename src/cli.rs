//! Command line interface.
//!
//! rssvc <command> <name> [options]
//!   install   install a new service
//!   remove    stop (if running) and delete a service
//!   start | stop | restart | pause | continue
//!   status    query the service state
//!   get       dump the service configuration
//!   list      list services managed by rssvc
//!   export    dump configuration to a TOML file
//!   import    create/update a service from a TOML file
//!   help | version

use std::io::Write;

use crate::config::{self, Config};
use crate::{util, VERSION};

/// Read a password from the console with echo disabled. Returns None when
/// stdin is not a console (redirected input) or reading failed.
fn read_password(prompt: &str) -> Option<String> {
    use windows::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, CONSOLE_MODE, ENABLE_ECHO_INPUT,
        STD_INPUT_HANDLE,
    };
    unsafe {
        let Ok(h) = GetStdHandle(STD_INPUT_HANDLE) else {
            return None;
        };
        let mut old = CONSOLE_MODE::default();
        if GetConsoleMode(h, &mut old).is_err() {
            return None; // not an interactive console
        }
        let _ = SetConsoleMode(h, CONSOLE_MODE(old.0 & !ENABLE_ECHO_INPUT.0));
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        let r = std::io::stdin().read_line(&mut line);
        let _ = SetConsoleMode(h, old); // restore echo
        eprintln!();
        r.ok()
            .map(|_| line.trim_end_matches(['\r', '\n']).to_string())
    }
}

pub fn run(args: &[String]) -> i32 {
    if args.is_empty() {
        print_help();
        return 0;
    }
    let cmd = args[0].as_str();
    let rest = &args[1..];
    match cmd {
        "install" => cmd_install(rest),
        "remove" | "uninstall" => cmd_remove(rest),
        "start" => simple_control(rest, "start"),
        "stop" => simple_control(rest, "stop"),
        "restart" => simple_control(rest, "restart"),
        "pause" => simple_control(rest, "pause"),
        "continue" | "resume" => simple_control(rest, "continue"),
        "status" => cmd_status(rest),
        "get" => cmd_get(rest),
        "list" => cmd_list(),
        "export" => cmd_export(rest),
        "import" => cmd_import(rest),
        "gui" => crate::gui::launch(), // never returns
        "help" | "--help" | "-h" | "-?" | "/?" => {
            print_help();
            0
        }
        "version" | "--version" | "-V" => {
            println!("rssvc v{VERSION} (Rust, {})", std::env::consts::ARCH);
            0
        }
        other => {
            eprintln!("rssvc: 未知命令 '{other}'\n");
            print_help();
            2
        }
    }
}

fn need_name(rest: &[String], cmd: &str) -> Option<String> {
    match rest.first() {
        Some(n) if !n.trim().is_empty() => Some(n.trim().to_string()),
        _ => {
            eprintln!("rssvc: 缺少服务名。用法: rssvc {cmd} <服务名> [...]\n提示: 运行 'rssvc help' 查看完整帮助。");
            None
        }
    }
}

fn err_exit(msg: &str) -> i32 {
    eprintln!("rssvc: {msg}");
    1
}

// ---------------------------------------------------------------- install --

fn cmd_install(args: &[String]) -> i32 {
    let mut name: Option<String> = None;
    let mut path: Option<String> = None;
    let mut params: Vec<String> = Vec::new();
    let mut cfg = Config::default();
    let mut user = String::new();
    let mut password = String::new();
    let mut stop_after_dashdash = false;

    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].clone();
        if !stop_after_dashdash && a == "--" {
            stop_after_dashdash = true;
            i += 1;
            continue;
        }
        if !stop_after_dashdash && a.starts_with("--") && a.len() > 2 {
            // Split "--key=value" if present.
            let (key, inline) = match a.find('=') {
                Some(p) => (a[..p].to_string(), Some(a[p + 1..].to_string())),
                None => (a.clone(), None),
            };
            let mut val = || -> Option<String> {
                if let Some(v) = inline.clone() {
                    return Some(v);
                }
                if i + 1 < args.len() {
                    i += 1;
                    Some(args[i].clone())
                } else {
                    None
                }
            };
            match key.as_str() {
                "--dir" | "--directory" => match val() {
                    Some(v) => cfg.app_directory = v,
                    None => return err_exit("--dir 需要一个参数"),
                },
                "--stdout" => match val() {
                    Some(v) => cfg.stdout = v,
                    None => return err_exit("--stdout 需要一个参数"),
                },
                "--stderr" => match val() {
                    Some(v) => cfg.stderr = v,
                    None => return err_exit("--stderr 需要一个参数"),
                },
                "--rotate-bytes" => match val().and_then(|v| v.parse::<u32>().ok()) {
                    Some(v) => cfg.rotate_bytes = v,
                    None => return err_exit("--rotate-bytes 需要数字参数(字节)"),
                },
                "--rotate-keep" => match val().and_then(|v| v.parse::<u32>().ok()) {
                    Some(v) => cfg.rotate_keep = v,
                    None => return err_exit("--rotate-keep 需要数字参数"),
                },
                "--env" => match val() {
                    Some(v) if v.contains('=') => cfg.environment.push(v),
                    _ => return err_exit("--env 需要 KEY=VALUE 形式的参数"),
                },
                "--startup" => match val().as_deref().and_then(Config::parse_startup) {
                    Some((s, d)) => {
                        cfg.startup = s;
                        cfg.delayed_autostart = d;
                    }
                    None => return err_exit("--startup 取值: auto | delayed | manual"),
                },
                "--priority" => match val().as_deref().and_then(Config::parse_priority) {
                    Some(p) => cfg.priority = p,
                    None => return err_exit("--priority 取值: realtime | high | above-normal | normal | below-normal | idle"),
                },
                "--user" => match val() {
                    Some(v) => user = v,
                    None => return err_exit("--user 需要一个参数"),
                },
                "--password" => {
                    // BUG-15 style hardening: prefer interactive input; the
                    // plaintext forms are still accepted but warn loudly.
                    if let Some(v) = inline.clone() {
                        eprintln!("warning: 密码以明文形式出现在命令行中 (进程列表/历史记录可见)。建议改用不带值的 --password 交互输入。");
                        password = v;
                    } else if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                        i += 1;
                        eprintln!("warning: 密码以明文形式出现在命令行中 (进程列表/历史记录可见)。建议改用不带值的 --password 交互输入。");
                        password = args[i].clone();
                    } else {
                        match read_password("请输入服务账户密码 (输入不回显): ") {
                            Some(p) if !p.is_empty() => password = p,
                            _ => return err_exit("--password 未读取到密码。"),
                        }
                    }
                }
                "--display" => match val() {
                    Some(v) => cfg.display_name = v,
                    None => return err_exit("--display 需要一个参数"),
                },
                "--description" | "--desc" => match val() {
                    Some(v) => cfg.description = v,
                    None => return err_exit("--description 需要一个参数"),
                },
                "--depends" => match val() {
                    Some(v) => {
                        for d in v.split(&[',', ';'][..]) {
                            let d = d.trim();
                            if !d.is_empty() {
                                cfg.dependencies.push(d.to_string());
                            }
                        }
                    }
                    None => return err_exit("--depends 需要一个参数(逗号分隔)"),
                },
                "--restart-delay" => match val().and_then(|v| v.parse::<u32>().ok()) {
                    Some(v) => cfg.restart_delay_ms = v,
                    None => return err_exit("--restart-delay 需要数字参数(毫秒)"),
                },
                "--throttle" => match val().and_then(|v| v.parse::<u32>().ok()) {
                    Some(v) => cfg.throttle_ms = v,
                    None => return err_exit("--throttle 需要数字参数(毫秒)"),
                },
                "--max-restarts" => match val().and_then(|v| v.parse::<u32>().ok()) {
                    Some(v) => cfg.max_restarts = v,
                    None => return err_exit("--max-restarts 需要数字参数"),
                },
                "--stop-timeout-console" => match val().and_then(|v| v.parse::<u32>().ok()) {
                    Some(v) => cfg.stop_timeout_console = v,
                    None => return err_exit("--stop-timeout-console 需要数字参数(毫秒)"),
                },
                "--stop-timeout-window" => match val().and_then(|v| v.parse::<u32>().ok()) {
                    Some(v) => cfg.stop_timeout_window = v,
                    None => return err_exit("--stop-timeout-window 需要数字参数(毫秒)"),
                },
                "--stop-timeout-threads" => match val().and_then(|v| v.parse::<u32>().ok()) {
                    Some(v) => cfg.stop_timeout_threads = v,
                    None => return err_exit("--stop-timeout-threads 需要数字参数(毫秒)"),
                },
                "--skip-console" => cfg.stop_method_skip |= crate::runner::SKIP_CONSOLE,
                "--skip-window" => cfg.stop_method_skip |= crate::runner::SKIP_WINDOW,
                "--skip-threads" => cfg.stop_method_skip |= crate::runner::SKIP_THREADS,
                "--skip-terminate" => cfg.stop_method_skip |= crate::runner::SKIP_TERMINATE,
                other => return err_exit(&format!("未知选项 {other}(运行 'rssvc help' 查看帮助)")),
            }
            i += 1;
            continue;
        }
        // Positional: name, path, then free-form parameters.
        if name.is_none() {
            name = Some(a.clone());
        } else if path.is_none() {
            path = Some(a.clone());
        } else {
            params.push(a);
        }
        i += 1;
    }

    let Some(name) = name else {
        eprintln!("rssvc: 用法: rssvc install <服务名> <程序路径> [参数...] [选项]");
        return 2;
    };
    let Some(path) = path else {
        eprintln!("rssvc: 用法: rssvc install <服务名> <程序路径> [参数...] [选项]");
        return 2;
    };

    if let Err(e) = util::validate_service_name(&name) {
        return err_exit(&e);
    }

    if !std::path::Path::new(&path).is_file() {
        return err_exit(&format!("程序路径不存在: {path}"));
    }
    // canonicalize_plain strips the \\?\ verbatim prefix on Windows; raw
    // canonicalize() output would leak it into the registry and confuse
    // cmd.exe and programs that inspect their own path.
    cfg.application = util::canonicalize_plain(&path);
    // Re-quote each argument following the Windows argv rules so arguments
    // containing spaces survive the parse -> rejoin round trip exactly as
    // the user typed them.
    cfg.app_parameters = params
        .iter()
        .map(|p| util::quote_arg(p))
        .collect::<Vec<_>>()
        .join(" ");
    if cfg.app_directory.trim().is_empty() {
        cfg.app_directory = std::path::Path::new(&cfg.application)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
    }
    cfg.account = user;
    cfg.password = password;
    if cfg.display_name.trim().is_empty() {
        cfg.display_name = name.clone();
    }

    // Where is rssvc.exe itself? The service ImagePath points back at it.
    let self_exe = match std::env::current_exe() {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(_) => return err_exit("无法确定 rssvc.exe 自身路径"),
    };

    let scm = match crate::scm::open_manager(true) {
        Ok(h) => h,
        Err(e) => return err_exit(&e),
    };

    match crate::scm::create_rssvc_service(scm, &name, &cfg, &self_exe) {
        Ok(_svc) => {}
        Err(e) => return err_exit(&e),
    }

    // The service now exists. If the Parameters write fails we must roll the
    // half-created service back instead of leaving a configless shell behind.
    if let Err(e) = cfg.save_parameters(&name) {
        let rolled = (|| -> Result<(), String> {
            let svc = crate::scm::open_service(scm, &name, crate::scm::ACCESS_ALL)?;
            crate::scm::delete(&svc)
        })();
        return err_exit(&format!(
            "写入注册表配置失败: {e}{}",
            if rolled.is_ok() {
                "\n已回滚: 刚创建的服务已删除。"
            } else {
                "\n警告: 自动回滚失败, 请手动执行 rssvc remove "
            }
        ));
    }
    if let Err(e) = cfg.save_delayed_flag(&name) {
        eprintln!("warning: 设置延迟启动标志失败: {e}");
    }

    println!("服务 {name} 安装成功。");
    println!("  程序:    \"{}\" {}", cfg.application, cfg.app_parameters);
    if !cfg.stdout.is_empty() || !cfg.stderr.is_empty() {
        println!("  日志:    {} {}", cfg.stdout, cfg.stderr);
    }
    println!("启动服务: rssvc start {name}");
    0
}

// ----------------------------------------------------------------- remove --

fn cmd_remove(args: &[String]) -> i32 {
    let Some(name) = need_name(args, "remove") else {
        return 2;
    };
    let scm = match crate::scm::open_manager(true) {
        Ok(h) => h,
        Err(e) => return err_exit(&e),
    };
    let svc = match crate::scm::open_service(scm, &name, crate::scm::ACCESS_ALL) {
        Ok(s) => s,
        Err(e) => return err_exit(&e),
    };

    // Stop first if running.
    if let Ok(st) = crate::scm::query_status(&svc) {
        if st.dwCurrentState.0 != crate::scm::STATE_STOPPED {
            println!("服务正在运行, 先停止...");
            if let Err(e) = crate::scm::control(&svc, crate::scm::CONTROL_STOP) {
                eprintln!("warning: {e}");
            }
            if !crate::scm::wait_for_state(&svc, crate::scm::STATE_STOPPED, 30_000) {
                return err_exit("服务停止超时, 未删除。可稍后重试或手动处理。");
            }
        }
    }

    if let Err(e) = crate::scm::delete(&svc) {
        return err_exit(&e);
    }
    println!("服务 {name} 已删除。");
    0
}

// -------------------------------------------------------- simple controls --

fn simple_control(args: &[String], action: &str) -> i32 {
    let Some(name) = need_name(args, action) else {
        return 2;
    };
    let scm = match crate::scm::open_manager(true) {
        Ok(h) => h,
        Err(e) => return err_exit(&e),
    };
    let svc = match crate::scm::open_service(
        scm,
        &name,
        crate::scm::ACCESS_START
            | crate::scm::ACCESS_STOP
            | crate::scm::ACCESS_PAUSE
            | crate::scm::ACCESS_QUERY,
    ) {
        Ok(s) => s,
        Err(e) => return err_exit(&e),
    };

    match action {
        "start" => {
            if let Err(e) = crate::scm::start(&svc) {
                return err_exit(&e);
            }
            let _ = crate::scm::wait_for_state(&svc, crate::scm::STATE_RUNNING, 5000);
            print_status_line(&name, &svc);
            0
        }
        "stop" => {
            if let Err(e) = crate::scm::control(&svc, crate::scm::CONTROL_STOP) {
                return err_exit(&e);
            }
            if crate::scm::wait_for_state(&svc, crate::scm::STATE_STOPPED, 30_000) {
                println!("服务 {name} 已停止。");
                0
            } else {
                err_exit("服务停止超时(30 秒)。停止序列可能仍在进行, 稍后用 'rssvc status' 查看。")
            }
        }
        "restart" => {
            if let Ok(st) = crate::scm::query_status(&svc) {
                if st.dwCurrentState.0 != crate::scm::STATE_STOPPED {
                    if let Err(e) = crate::scm::control(&svc, crate::scm::CONTROL_STOP) {
                        return err_exit(&e);
                    }
                    if !crate::scm::wait_for_state(&svc, crate::scm::STATE_STOPPED, 30_000) {
                        return err_exit("服务停止超时, 无法重启。");
                    }
                }
            }
            if let Err(e) = crate::scm::start(&svc) {
                return err_exit(&e);
            }
            let _ = crate::scm::wait_for_state(&svc, crate::scm::STATE_RUNNING, 5000);
            print_status_line(&name, &svc);
            0
        }
        "pause" => {
            if let Err(e) = crate::scm::control(&svc, crate::scm::CONTROL_PAUSE) {
                return err_exit(&e);
            }
            let _ = crate::scm::wait_for_state(&svc, crate::scm::STATE_PAUSED, 30_000);
            print_status_line(&name, &svc);
            0
        }
        "continue" => {
            if let Err(e) = crate::scm::control(&svc, crate::scm::CONTROL_CONTINUE) {
                return err_exit(&e);
            }
            let _ = crate::scm::wait_for_state(&svc, crate::scm::STATE_RUNNING, 30_000);
            print_status_line(&name, &svc);
            0
        }
        _ => unreachable!(),
    }
}

fn print_status_line(name: &str, svc: &crate::scm::Service) {
    match crate::scm::query_status(svc) {
        Ok(st) => {
            let state = match st.dwCurrentState.0 {
                crate::scm::STATE_STOPPED => "已停止".to_string(),
                crate::scm::STATE_START_PENDING => "启动中".to_string(),
                crate::scm::STATE_STOP_PENDING => "停止中".to_string(),
                crate::scm::STATE_RUNNING => "运行中".to_string(),
                crate::scm::STATE_PAUSE_PENDING => "暂停中".to_string(),
                crate::scm::STATE_PAUSED => "已暂停".to_string(),
                _ => format!("未知({})", st.dwCurrentState.0),
            };
            println!("服务 {name}: {state}");
        }
        Err(e) => eprintln!("rssvc: {e}"),
    }
}

// ------------------------------------------------------------------ status --

fn cmd_status(args: &[String]) -> i32 {
    let Some(name) = need_name(args, "status") else {
        return 2;
    };
    let scm = match crate::scm::open_manager(false) {
        Ok(h) => h,
        Err(e) => return err_exit(&e),
    };
    let svc = match crate::scm::open_service(scm, &name, crate::scm::ACCESS_QUERY) {
        Ok(s) => s,
        Err(e) => return err_exit(&e),
    };
    print_status_line(&name, &svc);
    0
}

// --------------------------------------------------------------------- get --

fn cmd_get(args: &[String]) -> i32 {
    let Some(name) = need_name(args, "get") else {
        return 2;
    };
    match Config::load(&name) {
        Ok(cfg) => {
            config::print_config(&name, &cfg);
            0
        }
        Err(e) => err_exit(&format!("读取服务配置失败: {e}")),
    }
}

// -------------------------------------------------------------------- list --

fn cmd_list() -> i32 {
    use winreg::enums::*;
    use winreg::RegKey;
    let hk = RegKey::predef(HKEY_LOCAL_MACHINE);
    let services = match hk.open_subkey(r"SYSTEM\CurrentControlSet\Services") {
        Ok(k) => k,
        Err(e) => return err_exit(&format!("打开服务注册表失败: {e}")),
    };
    let mut found: Vec<(String, String)> = Vec::new();
    for name in services.enum_keys().flatten() {
        let ok = services
            .open_subkey(&name)
            .and_then(|k| k.get_value::<String, _>("ImagePath"))
            .map(|s| s.to_lowercase().contains("rssvc.exe"))
            .unwrap_or(false);
        if ok {
            let display = services
                .open_subkey(&name)
                .and_then(|k| k.get_value::<String, _>("DisplayName"))
                .unwrap_or_default();
            found.push((name, display));
        }
    }
    if found.is_empty() {
        println!("没有找到由 rssvc 管理的服务。");
        return 0;
    }
    println!("由 rssvc 管理的服务 ({}):", found.len());
    // Query live state best-effort.
    let scm = crate::scm::open_manager(false).ok();
    for (name, display) in found {
        let state = scm
            .as_ref()
            .and_then(|s| crate::scm::open_service(*s, &name, crate::scm::ACCESS_QUERY).ok())
            .and_then(|svc| crate::scm::query_status(&svc).ok())
            .map(|st| match st.dwCurrentState.0 {
                crate::scm::STATE_RUNNING => "运行中".to_string(),
                crate::scm::STATE_STOPPED => "已停止".to_string(),
                crate::scm::STATE_PAUSED => "已暂停".to_string(),
                _ => "其他".to_string(),
            })
            .unwrap_or_else(|| "?".to_string());
        if display.is_empty() {
            println!("  {name:<30} [{state}]");
        } else {
            println!("  {name:<30} [{state}] {display}");
        }
    }
    0
}

// ----------------------------------------------------------- export/import --

fn cmd_export(args: &[String]) -> i32 {
    let Some(name) = need_name(args, "export") else {
        return 2;
    };
    let cfg = match Config::load(&name) {
        Ok(c) => c,
        Err(e) => return err_exit(&format!("读取服务配置失败: {e}")),
    };
    let toml_str = match toml::to_string_pretty(&cfg) {
        Ok(s) => s,
        Err(e) => return err_exit(&format!("生成 TOML 失败: {e}")),
    };
    match args.get(1) {
        Some(file) => match std::fs::write(file, &toml_str) {
            Ok(_) => {
                println!("已导出服务 {name} 配置到 {file} (密码不会导出)。");
                0
            }
            Err(e) => err_exit(&format!("写入文件失败: {e}")),
        },
        None => {
            let mut out = std::io::stdout();
            let _ = out.write_all(toml_str.as_bytes());
            0
        }
    }
}

fn cmd_import(args: &[String]) -> i32 {
    let Some(name) = need_name(args, "import") else {
        return 2;
    };
    let Some(file) = args.get(1) else {
        eprintln!("rssvc: 用法: rssvc import <服务名> <配置.toml>");
        return 2;
    };
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => return err_exit(&format!("读取文件失败: {e}")),
    };
    let cfg: Config = match toml::from_str(&text) {
        Ok(c) => c,
        Err(e) => return err_exit(&format!("解析 TOML 失败: {e}")),
    };
    if cfg.application.trim().is_empty() {
        return err_exit("TOML 中缺少 application 字段");
    }
    // Normalize the path (strip verbatim prefix, resolve relative paths when
    // the target exists) so imported configs look the same as fresh installs.
    let mut cfg = cfg;
    cfg.application = util::canonicalize_plain(&cfg.application);
    if !cfg.account.trim().is_empty() && cfg.account.trim() != "LocalSystem" {
        eprintln!(
            "warning: TOML 中的 account ({}) 无法附带密码; 若创建服务失败, 请改用 install --user/--password。",
            cfg.account
        );
    }

    let scm = match crate::scm::open_manager(true) {
        Ok(h) => h,
        Err(e) => return err_exit(&e),
    };

    // Existing service -> update config; otherwise create it (ImagePath
    // pointing back at rssvc.exe).
    let existing = crate::scm::open_service(scm, &name, crate::scm::ACCESS_ALL).ok();
    if existing.is_none() {
        let self_exe = match std::env::current_exe() {
            Ok(p) => p.to_string_lossy().to_string(),
            Err(_) => return err_exit("无法确定 rssvc.exe 自身路径"),
        };
        if let Err(e) = crate::scm::create_rssvc_service(scm, &name, &cfg, &self_exe) {
            return err_exit(&e);
        }
    }

    // The service may have just been created above. Roll it back if the
    // Parameters write fails (same contract as `install`).
    if let Err(e) = cfg.save_parameters(&name) {
        if existing.is_none() {
            let rolled = (|| -> Result<(), String> {
                let svc = crate::scm::open_service(scm, &name, crate::scm::ACCESS_ALL)?;
                crate::scm::delete(&svc)
            })();
            let note = if rolled.is_ok() {
                "\n已回滚: 刚创建的服务已删除。"
            } else {
                "\n警告: 自动回滚失败, 请手动执行 rssvc remove"
            };
            return err_exit(&format!("写入注册表配置失败: {e}{note}"));
        }
        return err_exit(&format!("写入注册表配置失败: {e}"));
    }
    if let Err(e) = cfg.save_delayed_flag(&name) {
        eprintln!("warning: 设置延迟启动标志失败: {e}");
    }
    // Start type / display name / description / dependencies.
    if cfg.dependencies.is_empty() {
        eprintln!("提示: TOML 未提供 dependencies (依赖服务), 保留服务现有依赖不变。");
    }
    let svc = match crate::scm::open_service(scm, &name, crate::scm::ACCESS_CONFIG) {
        Ok(s) => s,
        Err(e) => return err_exit(&e),
    };
    if let Err(e) = crate::scm::change_config(&svc, &cfg) {
        eprintln!("warning: {e}");
    }

    println!("服务 {name} 配置导入完成。");
    println!(
        "提示: 若服务正在运行, 执行 'rssvc restart {}' 或发送 PARAMCHANGE 以应用新配置。",
        name
    );
    0
}

// -------------------------------------------------------------------- help --

fn print_help() {
    println!(
        r#"rssvc v{VERSION} - 轻量级 Windows 服务包装器 (NSSM 替代品, Rust 实现)

用法: rssvc <命令> <服务名> [参数] [选项]

命令:
  install <服务名> <程序路径> [参数...] [--选项]
            安装新服务 (程序路径之后、第一个 -- 之前的参数都会作为启动参数)
  remove <服务名>            停止并删除服务 (别名: uninstall)
  start <服务名>             启动服务
  stop <服务名>              停止服务
  restart <服务名>           重启服务
  pause <服务名>             暂停服务 (停止被包裹的应用, 服务保持"已暂停")
  continue <服务名>          恢复服务 (别名: resume)
  status <服务名>            查看服务状态
  get <服务名>               查看服务配置
  list                       列出本机上由 rssvc 管理的服务
  export <服务名> [文件]     导出配置为 TOML (用于备份/迁移, 不含密码)
  import <服务名> <文件>     从 TOML 导入配置 (服务不存在则自动创建)
  gui                       打开图形界面服务管理器 (双击 exe 同效)
  version                    显示版本
  help                       显示本帮助

install 常用选项:
  --dir <目录>                  工作目录 (默认: 程序所在目录)
  --stdout <文件>               stdout 日志文件
  --stderr <文件>               stderr 日志文件
  --rotate-bytes <字节>         日志按大小轮转阈值 (0=仅按日期轮转, 默认 10MB)
  --rotate-keep <份数>          保留的历史日志份数 (0=全部保留, 默认 10)
  --env <KEY=VALUE>             附加环境变量, 可重复
  --startup <auto|delayed|manual>  启动类型 (默认 auto; delayed=开机延迟自启)
  --priority <级别>             realtime|high|above-normal|normal|below-normal|idle
  --display <名称>              服务显示名称
  --description <文本>          服务描述
  --depends <服务1,服务2>       依赖的服务
  --user <账户> [--password <密码>]  以指定账户运行 (默认 LocalSystem);
                                --password 不带值时交互输入不回显 (推荐)
  --restart-delay <毫秒>        应用退出后重启延迟 (默认 1000)
  --throttle <毫秒>             运行时长小于该值视为异常崩溃 (默认 1500)
  --max-restarts <次数>         连续快速崩溃达到该次数后放弃 (0=不放弃, 默认 10)
  --stop-timeout-console <毫秒> CTRL_BREAK 等待超时 (默认 1500)
  --stop-timeout-window <毫秒>  WM_CLOSE 等待超时 (默认 1500)
  --stop-timeout-threads <毫秒> WM_QUIT 等待超时 (默认 1500)
  --skip-console|window|threads|terminate  跳过对应停止级别
  --                            之后的参数原样作为应用参数 (含 -- 开头的)

示例:
  rssvc install my-node "C:\Program Files\nodejs\node.exe" server.js --dir C:\myapp --stdout C:\logs\out.log
  rssvc install my-py C:\Python312\python.exe -m http.server 8000 --startup delayed
  rssvc start my-node
  rssvc list

说明:
  - install/remove/start/stop 等操作需要管理员权限。
  - 服务崩溃后自动重启: 正常退出延迟 restart-delay 重启; 若应用运行时间
    短于 throttle 视为崩溃循环, 指数退避(1s,2s,4s...上限 60s), 连续超过
    max-restarts 次后停止服务并报错, 避免空转。
  - 停止服务按 nssm 方式多级执行: CTRL_BREAK -> WM_CLOSE -> WM_QUIT -> 强杀。
"#
    );
}
