use nix::fcntl::{open, OFlag};
use nix::sys::stat::Mode;
use nix::sys::wait::waitpid;
use nix::unistd::{dup2, execvp, fork, pipe, close, ForkResult};
use std::env;
use std::ffi::{CString, CStr};
use std::io::{self, Write};
use std::os::unix::io::IntoRawFd;
use std::path::Path;

fn prompt() {
    print!("mysh> ");
    io::stdout().flush().unwrap();
}

fn split_pipeline(line: &str) -> Vec<&str> {
    line.split('|').map(|s| s.trim()).filter(|s| !s.is_empty()).collect()
}

fn tokenize(cmd: &str) -> Vec<String> {
    cmd.split_whitespace().map(|s| s.to_string()).collect()
}

fn extract_redirection(tokens: &[String]) -> (Vec<String>, Option<String>, Option<String>) {
    let mut args = Vec::new();
    let mut stdout_path = None;
    let mut stdin_path = None;

    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].as_str() {
            ">" => {
                if i + 1 < tokens.len() {
                    stdout_path = Some(tokens[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("syntax error: expected filename after >");
                    break;
                }
            }
            "<" => {
                if i + 1 < tokens.len() {
                    stdin_path = Some(tokens[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("syntax error: expected filename after <");
                    break;
                }
            }
            _ => {
                args.push(tokens[i].clone());
                i += 1;
            }
        }
    }

    (args, stdout_path, stdin_path)
}

fn exec_single_command(tokens: &[String]) {
    if tokens.is_empty() {
        return;
    }

    if tokens[0] == "cd" {
        let target = if tokens.len() > 1 {
            &tokens[1]
        } else {
            &env::var("HOME").unwrap_or_else(|_| "/".to_string())
        };
        if let Err(e) = env::set_current_dir(Path::new(target)) {
            eprintln!("cd: {}", e);
        }
        return;
    }

    if tokens[0] == "exit" {
        std::process::exit(0);
    }

    let (args, stdout_path, stdin_path) = extract_redirection(tokens);
    if args.is_empty() {
        return;
    }

    let cstr_args: Vec<CString> = args.iter().map(|s| CString::new(s.as_str()).unwrap()).collect();
    let prog = cstr_args[0].clone();

    match unsafe { fork() } {
        Ok(ForkResult::Child) => {
            // redirection in child
            if let Some(ref out) = stdout_path {
                let fd = open(out.as_str(), OFlag::O_CREAT | OFlag::O_WRONLY | OFlag::O_TRUNC, Mode::from_bits_truncate(0o644))
                    .expect("open out failed");
                dup2(fd, 1).expect("dup2 stdout failed");
                close(fd).ok();
            }

            if let Some(ref inp) = stdin_path {
                let fd = open(inp.as_str(), OFlag::O_RDONLY, Mode::empty()).expect("open in failed");
                dup2(fd, 0).expect("dup2 stdin failed");
                close(fd).ok();
            }


            let cprog: &CStr = &prog;
            let argv_ref: Vec<&CStr> = cstr_args.iter().map(|s| s.as_c_str()).collect();
            execvp(cprog, &argv_ref).unwrap_or_else(|err| {
                eprintln!("exec failed: {}", err);
                std::process::exit(1);
            });
        }
        Ok(ForkResult::Parent { child }) => {
            waitpid(child, None).ok();
        }
        Err(err) => {
            eprintln!("fork failed: {}", err);
        }
    }
}

fn exec_pipe_two(left_tokens: &[String], right_tokens: &[String]) {
    let (left_args, _left_stdout, left_stdin) = extract_redirection(left_tokens);
    let (right_args, right_stdout, right_stdin) = extract_redirection(right_tokens);

    if left_args.is_empty() || right_args.is_empty() {
        eprintln!("empty command around pipe");
        return;
    }

    let left_c: Vec<CString> = left_args.iter().map(|s| CString::new(s.as_str()).unwrap()).collect();
    let right_c: Vec<CString> = right_args.iter().map(|s| CString::new(s.as_str()).unwrap()).collect();

    let (rfd_owned, wfd_owned) = pipe().expect("pipe failed");
    let rfd = rfd_owned.into_raw_fd();
    let wfd = wfd_owned.into_raw_fd();

    match unsafe { fork() } {
        Ok(ForkResult::Child) => {
            dup2(wfd, 1).expect("dup2 left stdout failed");

            if let Some(ref inp) = left_stdin {
                let fd = open(inp.as_str(), OFlag::O_RDONLY, Mode::empty()).expect("open left stdin failed");
                dup2(fd, 0).expect("dup2 left stdin failed");
                close(fd).ok();
            }

            close(rfd).ok();
            close(wfd).ok();

            let prog: &CStr = left_c[0].as_c_str();
            let argv_ref: Vec<&CStr> = left_c.iter().map(|s| s.as_c_str()).collect();
            execvp(prog, &argv_ref).unwrap_or_else(|err| {
                eprintln!("left exec failed: {}", err);
                std::process::exit(1);
            });
        }
        Ok(ForkResult::Parent { child: left_child }) => {
            match unsafe { fork() } {
                Ok(ForkResult::Child) => {
                    dup2(rfd, 0).expect("dup2 right stdin failed");

                    if let Some(ref out) = right_stdout {
                        let fd = open(out.as_str(), OFlag::O_CREAT | OFlag::O_WRONLY | OFlag::O_TRUNC, Mode::from_bits_truncate(0o644)).expect("open right out failed");
                        dup2(fd, 1).expect("dup2 right stdout failed");
                        close(fd).ok();
                    }

                    if let Some(ref inp) = right_stdin {
                        let fd = open(inp.as_str(), OFlag::O_RDONLY, Mode::empty()).expect("open right stdin failed");
                        dup2(fd, 0).expect("dup2 right stdin failed");
                        close(fd).ok();
                    }

                    close(rfd).ok();
                    close(wfd).ok();

                    let prog: &CStr = right_c[0].as_c_str();
                    let argv_ref: Vec<&CStr> = right_c.iter().map(|s| s.as_c_str()).collect();
                    execvp(prog, &argv_ref).unwrap_or_else(|err| {
                        eprintln!("right exec failed: {}", err);
                        std::process::exit(1);
                    });
                }
                Ok(ForkResult::Parent { child: right_child }) => {
                    close(rfd).ok();
                    close(wfd).ok();
                    waitpid(left_child, None).ok();
                    waitpid(right_child, None).ok();
                }
                Err(err) => eprintln!("fork failed for right: {}", err),
            }
        }
        Err(err) => eprintln!("fork failed for left: {}", err),
    }
}

fn main() {
    loop {
        prompt();
        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            println!();
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let pipeline = split_pipeline(line);
        if pipeline.len() == 1 {
            let tokens = tokenize(pipeline[0]);
            exec_single_command(&tokens);
        } else if pipeline.len() == 2 {
            let left_tokens = tokenize(pipeline[0]);
            let right_tokens = tokenize(pipeline[1]);
            exec_pipe_two(&left_tokens, &right_tokens);
        } else {
            eprintln!("Only single pipe supported in this version (extendable).");
        }
    }
}
