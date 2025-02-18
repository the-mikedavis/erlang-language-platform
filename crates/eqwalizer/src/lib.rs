/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::prelude::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use ast::Error;
use ast::Pos;
use elp_base_db::limit_logged_string;
use elp_base_db::ModuleName;
use elp_base_db::ProjectId;
use elp_types_db::eqwalizer::types::Type;
pub use elp_types_db::eqwalizer::EqwalizerDiagnostic;
use fxhash::FxHashMap;
use lazy_static::lazy_static;
use parking_lot::Mutex;
use tempfile::Builder;
use tempfile::TempPath;

pub mod ipc;
use ipc::IpcHandle;
use ipc::MsgFromEqWAlizer;
use ipc::MsgToEqWAlizer;

use crate::ipc::EqWAlizerASTFormat;

pub mod analyses;
pub mod ast;
pub use elp_types_db::eqwalizer::types;
pub use elp_types_db::IncludeGenerated;

#[derive(Clone, Eq, PartialEq, Debug)]
pub enum Mode {
    Cli,
    Server,
    Shell,
}
impl Mode {
    fn to_env_var(&self) -> &str {
        match self {
            Mode::Cli => "elp_cli",
            Mode::Server => "elp_ide",
            Mode::Shell => "shell",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EqwalizerConfig {
    pub fault_tolerance: Option<bool>,
    pub occurrence_typing: Option<bool>,
    pub clause_coverage: Option<bool>,
    pub report_bad_maps: Option<bool>,
    pub overloaded_spec_dynamic_result: Option<bool>,
    pub report_dynamic_lambdas: Option<bool>,
}
impl EqwalizerConfig {
    fn set_cmd_env(&self, cmd: &mut Command) {
        self.fault_tolerance
            .map(|cfg| cmd.env("EQWALIZER_TOLERATE_ERRORS", cfg.to_string()));
        self.occurrence_typing
            .map(|cfg| cmd.env("EQWALIZER_EQWATER", cfg.to_string()));
        self.clause_coverage
            .map(|cfg| cmd.env("EQWALIZER_CLAUSE_COVERAGE", cfg.to_string()));
        self.report_bad_maps
            .map(|cfg| cmd.env("EQWALIZER_REPORT_BAD_MAPS", cfg.to_string()));
        self.overloaded_spec_dynamic_result
            .map(|cfg| cmd.env("EQWALIZER_OVERLOADED_SPEC_DYNAMIC_RESULT", cfg.to_string()));
        self.report_dynamic_lambdas
            .map(|cfg| cmd.env("EQWALIZER_REPORT_DYNAMIC_LAMBDAS", cfg.to_string()));
    }

    pub fn default_test() -> EqwalizerConfig {
        EqwalizerConfig {
            fault_tolerance: Some(false),
            occurrence_typing: Some(true),
            clause_coverage: Some(false),
            report_bad_maps: Some(false),
            overloaded_spec_dynamic_result: Some(false),
            report_dynamic_lambdas: Some(false),
        }
    }
}

// Bundle file with command to make sure it's not removed too early
#[derive(Clone)]
pub struct Eqwalizer {
    pub mode: Mode,
}

#[derive(Clone)]
pub struct EqwalizerExe {
    cmd: PathBuf,
    args: Vec<OsString>,
    // Used only for the Drop implementation
    _file: Option<Arc<TempPath>>,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum EqwalizerDiagnostics {
    Diagnostics {
        errors: FxHashMap<String, Vec<EqwalizerDiagnostic>>,
        type_info: FxHashMap<String, Vec<(Pos, Type)>>,
    },
    NoAst {
        module: String,
    },
    Error(String),
}

impl Default for EqwalizerDiagnostics {
    fn default() -> Self {
        EqwalizerDiagnostics::Diagnostics {
            errors: Default::default(),
            type_info: Default::default(),
        }
    }
}

impl EqwalizerDiagnostics {
    pub fn combine(mut self, other: Self) -> Self {
        match &mut self {
            EqwalizerDiagnostics::NoAst {
                module: self_module,
            } => match &other {
                EqwalizerDiagnostics::NoAst {
                    module: other_module,
                } => {
                    if other_module > self_module {
                        self
                    } else {
                        other
                    }
                }
                _ => self,
            },
            EqwalizerDiagnostics::Error(_) => self,
            EqwalizerDiagnostics::Diagnostics { errors, type_info } => match other {
                EqwalizerDiagnostics::Diagnostics {
                    errors: other_errors,
                    type_info: other_type_info,
                } => {
                    errors.extend(other_errors.into_iter());
                    type_info.extend(other_type_info.into_iter());
                    self
                }
                EqwalizerDiagnostics::Error(_) => other.clone(),
                EqwalizerDiagnostics::NoAst { .. } => other.clone(),
            },
        }
    }
}

pub trait DbApi {
    fn eqwalizing_start(&self, module: String);
    fn eqwalizing_done(&self, module: String);
    fn set_module_ipc_handle(&self, module: ModuleName, handle: Option<Arc<Mutex<IpcHandle>>>);
    fn module_ipc_handle(&self, module: ModuleName) -> Option<Arc<Mutex<IpcHandle>>>;
}

#[salsa::query_group(EqwalizerDiagnosticsDatabaseStorage)]
pub trait EqwalizerDiagnosticsDatabase: ast::db::EqwalizerASTDatabase + DbApi {
    #[salsa::input]
    fn eqwalizer_config(&self) -> Arc<EqwalizerConfig>;

    fn module_diagnostics(
        &self,
        project_id: ProjectId,
        module: String,
    ) -> (Arc<EqwalizerDiagnostics>, Instant);
}

impl Default for Eqwalizer {
    fn default() -> Self {
        Self { mode: Mode::Server }
    }
}

lazy_static! {
    // We make a static version of the eqwalizer executable environment to
    // - Prevent race conditions in tests from the temporary file creation
    //   process (T182801661)
    // - Speed up tests, since we create a RootDatabase once per test
    //   needing the erlang service
    // We wrap it in an Arc to make sure it never goes out of scope,
    // triggering the Drop handler, until the programme exits.
    // It has a Mutex so it can be updated if the operating systen deletes the file
    // for a long-running ELP server.
    static ref EQWALIZER_EXE: Option<Arc<Mutex<EqwalizerExe>>> = EqwalizerExe::ensure_exe();
}

impl EqwalizerExe {
    // Identify the required Eqwalizer executable, and ensure it is
    // available on the file system
    fn ensure_exe() -> Option<Arc<Mutex<Self>>> {
        let env = env::var("ELP_EQWALIZER_PATH");
        let (path, ext, temp_file) = if let Ok(path) = env {
            let path = PathBuf::from(path);
            let ext = path
                .extension()
                .unwrap_or_default()
                .to_str()
                .unwrap()
                .to_string();
            (path, ext, None)
        } else if let Ok(extension) = env::var("ELP_EQWALIZER_EXT") {
            let mut p = PathBuf::from(env!("OUT_DIR"));
            p.push("eqwalizer");
            let eqwalizer_src = fs::read(p).expect("couldn't read eqwalizer bytes");
            let mut temp_file = Builder::new()
                .prefix("eqwalizer")
                .tempfile()
                .expect("can't create eqwalizer temp executable");
            temp_file
                .write_all(&eqwalizer_src)
                .expect("can't create eqwalizer temp executable");

            let temp_file = temp_file.into_temp_path();

            let mut perm = fs::metadata(&temp_file)
                .expect("can't create eqwalizer temp executable")
                .permissions();
            perm.set_mode(0o755);
            fs::set_permissions(&temp_file, perm).expect("can't create eqwalizer temp executable");

            (temp_file.to_path_buf(), extension, Some(temp_file))
        } else {
            return None;
        };

        let (cmd, args) = match ext.as_str() {
            "jar" => (
                "java".into(),
                vec!["-Xss20M".into(), "-jar".into(), path.into()],
            ),
            "" => (path.into(), vec![]),
            _ => panic!("Unknown eqwalizer executable {:?}", path),
        };

        Some(Arc::new(Mutex::new(Self {
            cmd,
            args,
            _file: temp_file.map(Arc::new),
        })))
    }

    pub fn cmd(&self) -> Command {
        let mut cmd = Command::new(&self.cmd);
        cmd.args(&self.args);
        cmd
    }
}

impl Eqwalizer {
    fn cmd(&self) -> Option<Command> {
        let exe = EQWALIZER_EXE.as_ref()?.lock();
        Some(exe.cmd())
    }

    pub fn typecheck(
        &self,
        db: &dyn EqwalizerDiagnosticsDatabase,
        project_id: ProjectId,
        modules: Vec<&str>,
    ) -> EqwalizerDiagnostics {
        let Some(mut cmd) = self.cmd() else {
            return EqwalizerDiagnostics::Diagnostics {
                errors: Default::default(),
                type_info: Default::default(),
            };
        };
        db.eqwalizer_config().set_cmd_env(&mut cmd);
        cmd.arg("ipc");
        cmd.args(modules);
        cmd.env("EQWALIZER_MODE", self.mode.to_env_var());

        match do_typecheck(cmd, db, project_id) {
            Ok(diags) => diags,
            Err(err) => EqwalizerDiagnostics::Error(format!("{:?}", err)),
        }
    }
}

fn do_typecheck(
    mut cmd: Command,
    db: &dyn EqwalizerDiagnosticsDatabase,
    project_id: ProjectId,
) -> Result<EqwalizerDiagnostics, anyhow::Error> {
    // Never cache the results of this function
    db.salsa_runtime().report_untracked_read();
    let handle = Arc::new(Mutex::new(
        IpcHandle::from_command(&mut cmd)
            .with_context(|| format!("starting eqWAlizer process: {:?}", cmd))?,
    ));
    let mut diagnostics = EqwalizerDiagnostics::default();
    loop {
        db.unwind_if_cancelled();
        let msg = handle.lock().receive()?;
        match msg {
            MsgFromEqWAlizer::EnteringModule { module } => {
                let module_name = ModuleName::new(&module);
                db.set_module_ipc_handle(module_name.clone(), Some(handle.clone()));
                let diags = db.module_diagnostics(project_id, module).0;
                db.set_module_ipc_handle(module_name, None);
                diagnostics = diagnostics.combine((*diags).clone());
                match diagnostics {
                    EqwalizerDiagnostics::Error(_) | EqwalizerDiagnostics::NoAst { .. } => {
                        return Ok(diagnostics);
                    }
                    EqwalizerDiagnostics::Diagnostics { .. } => (),
                }
                handle.lock().send(&MsgToEqWAlizer::ELPExitingModule)?;
            }
            MsgFromEqWAlizer::Done { .. } => {
                return Ok(diagnostics);
            }
            msg => {
                log::warn!(
                    "received unexpected message from eqwalizer, ignoring: {}",
                    limit_logged_string(&format!("{:?}", msg))
                )
            }
        }
    }
}

fn module_diagnostics(
    db: &dyn EqwalizerDiagnosticsDatabase,
    project_id: ProjectId,
    module: String,
) -> (Arc<EqwalizerDiagnostics>, Instant) {
    // A timestamp is added to the return value to force Salsa to store new
    // diagnostics, and not attempt to back-date them if they are equal to
    // the memoized ones.
    let timestamp = Instant::now();
    // Dummy read eqWAlizer config for Salsa
    // Ideally, the config should be passed per module to eqWAlizer instead
    // of being set in the command's environment
    let _ = db.eqwalizer_config();
    match get_module_diagnostics(db, project_id, module.clone()) {
        Ok(diag) => (Arc::new(diag), timestamp),
        Err(err) => (
            Arc::new(EqwalizerDiagnostics::Error(format!(
                "eqWAlizing module {}:\n{}",
                module, err
            ))),
            timestamp,
        ),
    }
}

fn get_module_diagnostics(
    db: &dyn EqwalizerDiagnosticsDatabase,
    project_id: ProjectId,
    module: String,
) -> Result<EqwalizerDiagnostics, anyhow::Error> {
    let handle_mutex = db
        .module_ipc_handle(ModuleName::new(&module))
        .ok_or(anyhow::Error::msg(format!(
            "no eqWAlizer handle for module {}",
            module
        )))?;
    let mut handle = handle_mutex.lock();
    handle.send(&MsgToEqWAlizer::ELPEnteringModule)?;
    loop {
        db.unwind_if_cancelled();
        match handle.receive()? {
            MsgFromEqWAlizer::GetAstBytes { module, format } => {
                log::debug!(
                    "received from eqwalizer: GetAstBytes for module {} (format = {:?})",
                    module,
                    format
                );
                let module_name = ModuleName::new(&module);
                let ast = {
                    match format {
                        EqWAlizerASTFormat::ConvertedForms => {
                            db.converted_ast_bytes(project_id, module_name)
                        }
                        EqWAlizerASTFormat::TransitiveStub => {
                            db.transitive_stub_bytes(project_id, module_name)
                        }
                    }
                };
                match ast {
                    Ok(ast_bytes) => {
                        log::debug!(
                            "sending to eqwalizer: GetAstBytesReply for module {}",
                            module
                        );
                        let ast_bytes_len = ast_bytes.len().try_into()?;
                        let reply = &MsgToEqWAlizer::GetAstBytesReply { ast_bytes_len };
                        handle.send(reply)?;
                        handle.receive_newline()?;
                        handle.send_bytes(&ast_bytes).with_context(|| {
                            format!(
                                "sending to eqwalizer: bytes for module {} (format = {:?})",
                                module, format
                            )
                        })?;
                    }
                    Err(Error::ModuleNotFound(_)) => {
                        log::debug!(
                            "module not found, sending to eqwalizer: empty GetAstBytesReply for module {}",
                            module
                        );
                        let ast_bytes_len = 0;
                        let reply = &MsgToEqWAlizer::GetAstBytesReply { ast_bytes_len };
                        handle.send(reply)?;
                        handle.receive_newline()?;
                    }
                    Err(Error::ParseError) => {
                        log::debug!(
                            "parse error, sending to eqwalizer: CannotCompleteRequest for module {}",
                            module
                        );
                        let reply = &MsgToEqWAlizer::CannotCompleteRequest;
                        handle.send(reply)?;
                        return Ok(EqwalizerDiagnostics::NoAst { module });
                    }
                    Err(err) => {
                        log::debug!(
                            "error {} sending to eqwalizer: CannotCompleteRequest for module {}",
                            err,
                            module
                        );
                        let reply = &MsgToEqWAlizer::CannotCompleteRequest;
                        handle.send(reply)?;
                        return Ok(EqwalizerDiagnostics::Error(err.to_string()));
                    }
                }
            }
            MsgFromEqWAlizer::EqwalizingStart { module } => db.eqwalizing_start(module),
            MsgFromEqWAlizer::EqwalizingDone { module } => db.eqwalizing_done(module),
            MsgFromEqWAlizer::Done {
                diagnostics,
                type_info,
            } => {
                log::debug!(
                    "received from eqwalizer: Done with diagnostics length {}",
                    diagnostics.len()
                );
                return Ok(EqwalizerDiagnostics::Diagnostics {
                    errors: diagnostics,
                    type_info,
                });
            }
            MsgFromEqWAlizer::Dependencies { modules } => {
                modules.iter().for_each(|module| {
                    let module = ModuleName::new(module);
                    _ = db.transitive_stub_bytes(project_id, module);
                });
            }
            msg => {
                log::warn!(
                    "received unexpected message from eqwalizer, ignoring: {}",
                    limit_logged_string(&format!("{:?}", msg))
                )
            }
        }
    }
}
