/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::ErrorKind;
use std::io::Write;
use std::process::Child;
use std::process::ChildStdin;
use std::process::ChildStdout;
use std::process::Command;
use std::process::Stdio;
use std::thread;
use std::time::Duration;

use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use elp_base_db::limit_logged_string;
use elp_base_db::ModuleName;
use elp_types_db::eqwalizer::types::Type;
use elp_types_db::eqwalizer::EqwalizerDiagnostic;
use fxhash::FxHashMap;
use serde::Deserialize;
use serde::Serialize;
use stdx::JodChild;
use timeout_readwrite::TimeoutReader;
use timeout_readwrite::TimeoutWriter;

use crate::ast::Pos;

#[derive(Deserialize, Debug)]
pub enum EqWAlizerASTFormat {
    ConvertedForms,
    TransitiveStub,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "tag", content = "content")]
pub enum MsgFromEqWAlizer {
    EnteringModule {
        module: ModuleName,
    },
    GetAstBytes {
        module: ModuleName,
        format: EqWAlizerASTFormat,
    },
    EqwalizingStart {
        module: ModuleName,
    },
    EqwalizingDone {
        module: ModuleName,
    },
    Dependencies {
        modules: Vec<ModuleName>,
    },
    Done {
        diagnostics: FxHashMap<ModuleName, Vec<EqwalizerDiagnostic>>,
        type_info: FxHashMap<ModuleName, Vec<(Pos, Type)>>,
    },
}

#[derive(Serialize, Debug)]
#[serde(tag = "tag", content = "content")]
pub enum MsgToEqWAlizer {
    ELPEnteringModule,
    ELPExitingModule,
    GetAstBytesReply { ast_bytes_len: u32 },
    CannotCompleteRequest,
}

pub struct IpcHandle {
    writer: BufWriter<TimeoutWriter<ChildStdin>>,
    reader: BufReader<TimeoutReader<ChildStdout>>,
    _child_for_drop: JodChild,
}

const WRITE_TIMEOUT: Duration = Duration::from_secs(240);
const READ_TIMEOUT: Duration = Duration::from_secs(240);

impl IpcHandle {
    fn spawn_cmd(cmd: &mut Command) -> Result<Child> {
        // Spawn can fail due to a race condition with the creation/closing of the
        // eqWAlizer executable, so we retry until the file is properly closed.
        // See https://github.com/rust-lang/rust/issues/114554
        loop {
            match cmd.spawn() {
                Ok(c) => return Ok(c),
                Err(err) => {
                    if err.kind() == ErrorKind::ExecutableFileBusy {
                        thread::sleep(Duration::from_millis(10));
                    } else {
                        // Provide extra debugging detail, to track down T198872667
                        let command_str = cmd.get_program();
                        let attr = fs::metadata(command_str);
                        let error_str = format!(
                            "err: {}, command_str: {:?}, cmd: {:?}, meta_data: {:?}",
                            err, command_str, cmd, &attr
                        );
                        // Show up in error log
                        log::error!("{}", limit_logged_string(&error_str));
                        // And show up as an eqwalizer error
                        bail!(error_str)
                    }
                }
            }
        }
    }

    pub fn from_command(cmd: &mut Command) -> Result<Self> {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // for debugging purposes
            .stderr(Stdio::inherit());

        let mut child = Self::spawn_cmd(cmd)?;
        let stdin = child
            .stdin
            .take()
            .context("failed to get stdin for eqwalizer process")?;
        let stdout = child
            .stdout
            .take()
            .context("failed to get stdout for eqwalizer process")?;

        let _child_for_drop = JodChild(child);
        let writer = BufWriter::new(TimeoutWriter::new(stdin, WRITE_TIMEOUT));
        let reader = BufReader::new(TimeoutReader::new(stdout, READ_TIMEOUT));

        Ok(Self {
            writer,
            reader,
            _child_for_drop,
        })
    }

    pub fn receive(&mut self) -> Result<MsgFromEqWAlizer> {
        let buf = self.receive_line().context("receiving message")?;
        let deserialized = serde_json::from_str(&buf)
            .with_context(|| format!("parsing for eqwalizer: {buf:?}"))?;
        Ok(deserialized)
    }

    pub fn receive_newline(&mut self) -> Result<()> {
        let _ = self.receive_line().context("receiving newline")?;
        Ok(())
    }

    pub fn send(&mut self, msg: &MsgToEqWAlizer) -> Result<()> {
        let msg = serde_json::to_string(msg).expect("failed to serialize msg to eqwalizer");
        writeln!(self.writer, "{}", msg).with_context(|| format!("writing message: {:?}", msg))?;
        self.writer
            .flush()
            .with_context(|| format!("flushing message: {:?}", msg))?;
        Ok(())
    }

    pub fn send_bytes(&mut self, msg: &[u8]) -> Result<()> {
        // Don't exceed pipe buffer size on Mac or Linux
        // https://unix.stackexchange.com/a/11954/147568
        let chunk_size = 65_536;
        for (idx, chunk) in msg.chunks(chunk_size).enumerate() {
            self.writer
                .write_all(chunk)
                .with_context(|| format!("writing bytes chunk {} of size {}", idx, chunk.len()))?;
        }
        self.writer
            .flush()
            .with_context(|| format!("flushing bytes of size {}", msg.len()))?;
        Ok(())
    }

    fn receive_line(&mut self) -> Result<String> {
        let mut buf = String::new();
        self.reader
            .read_line(&mut buf)
            .context("failed read_line from eqwalizer stdout")?;
        Ok(buf)
    }
}
