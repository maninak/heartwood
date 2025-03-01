#![allow(clippy::collapsible_else_if)]
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::{env, fs, io, mem};

use snapbox::cmd::{Command, OutputAssert};
use snapbox::{Assert, Substitutions};
use thiserror::Error;

/// Error lines in the CLI are prefixed with this string.
const ERROR_PREFIX: &str = "✗";

#[derive(Error, Debug)]
pub enum Error {
    #[error("parsing failed")]
    Parse,
    #[error("test file not found: {0:?}")]
    TestNotFound(PathBuf),
    #[error("i/o: {0}")]
    Io(#[from] io::Error),
    #[error("snapbox: {0}")]
    Snapbox(#[from] snapbox::Error),
}

#[derive(Debug, PartialEq, Eq)]
enum ExitStatus {
    Success,
    Failure,
}

/// A test which may contain multiple assertions.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Test {
    /// Human-readable context around the test. Functions as documentation.
    context: Vec<String>,
    /// Test assertions to run.
    assertions: Vec<Assertion>,
}

/// An assertion is a command to run with an expected output.
#[derive(Debug, PartialEq, Eq)]
pub struct Assertion {
    /// The test file that contains this assertion.
    path: PathBuf,
    /// Name of command to run, eg. `git`.
    command: String,
    /// Command arguments, eg. `["push"]`.
    args: Vec<String>,
    /// Expected output (stdout or stderr).
    expected: String,
    /// Expected exit status.
    exit: ExitStatus,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct TestFormula {
    /// Current working directory to run the test in.
    cwd: PathBuf,
    /// Environment to pass to the test.
    env: HashMap<String, String>,
    /// Tests to run.
    tests: Vec<Test>,
    /// Output substitutions.
    subs: Substitutions,
}

impl TestFormula {
    pub fn new() -> Self {
        Self {
            cwd: PathBuf::new(),
            env: HashMap::new(),
            tests: Vec::new(),
            subs: Substitutions::new(),
        }
    }

    pub fn cwd(&mut self, path: impl AsRef<Path>) -> &mut Self {
        self.cwd = path.as_ref().into();
        self
    }

    pub fn env(&mut self, key: impl Into<String>, val: impl Into<String>) -> &mut Self {
        self.env.insert(key.into(), val.into());
        self
    }

    pub fn envs<K: ToString, V: ToString>(
        &mut self,
        envs: impl IntoIterator<Item = (K, V)>,
    ) -> &mut Self {
        for (k, v) in envs {
            self.env.insert(k.to_string(), v.to_string());
        }
        self
    }

    pub fn file(&mut self, path: impl AsRef<Path>) -> Result<&mut Self, Error> {
        let path = path.as_ref();
        let contents = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Err(Error::TestNotFound(path.to_path_buf()));
            }
            Err(err) => return Err(err.into()),
        };
        self.read(path, io::Cursor::new(contents))
    }

    pub fn read(&mut self, path: &Path, r: impl io::BufRead) -> Result<&mut Self, Error> {
        let mut test = Test::default();
        let mut fenced = false; // Whether we're inside a fenced code block.

        for line in r.lines() {
            let line = line?;

            if line.starts_with("```") {
                if fenced {
                    // End existing code block.
                    self.tests.push(mem::take(&mut test));
                }
                fenced = !fenced;

                continue;
            }

            if fenced {
                if let Some(line) = line.strip_prefix('$') {
                    let line = line.trim();
                    let parts = shlex::split(line).ok_or(Error::Parse)?;
                    let (cmd, args) = parts.split_first().ok_or(Error::Parse)?;

                    test.assertions.push(Assertion {
                        path: path.to_path_buf(),
                        command: cmd.to_owned(),
                        args: args.to_owned(),
                        expected: String::new(),
                        exit: ExitStatus::Success,
                    });
                } else if let Some(test) = test.assertions.last_mut() {
                    if line.starts_with(ERROR_PREFIX) {
                        test.exit = ExitStatus::Failure;
                    }
                    test.expected.push_str(line.as_str());
                    test.expected.push('\n');
                } else {
                    return Err(Error::Parse);
                }
            } else {
                test.context.push(line);
            }
        }
        Ok(self)
    }

    #[allow(dead_code)]
    pub fn substitute(
        &mut self,
        value: &'static str,
        other: impl Into<Cow<'static, str>>,
    ) -> Result<&mut Self, Error> {
        self.subs.insert(value, other)?;
        Ok(self)
    }

    pub fn run(&mut self) -> Result<bool, io::Error> {
        let assert = Assert::new().substitutions(self.subs.clone());

        fs::create_dir_all(&self.cwd)?;

        for test in &self.tests {
            for assertion in &test.assertions {
                let path = assertion
                    .path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or(String::from("<none>"));
                let cmd = if assertion.command == "rad" {
                    snapbox::cmd::cargo_bin("rad")
                } else if assertion.command == "cd" {
                    let path: PathBuf = assertion.args.first().unwrap().into();
                    let path = self.cwd.join(path);

                    // TODO: Add support for `..` and `/`
                    // TODO: Error if more than one args are given.

                    if !path.exists() {
                        return Err(io::Error::new(
                            io::ErrorKind::NotFound,
                            format!("cd: '{}' does not exist", path.display()),
                        ));
                    }
                    self.cwd = path;

                    continue;
                } else {
                    PathBuf::from(&assertion.command)
                };
                log::debug!(target: "test", "{path}: Running `{}` in `{}`..", cmd.display(), self.cwd.display());

                if !self.cwd.exists() {
                    log::error!(target: "test", "{path}: Directory {} does not exist..", self.cwd.display());
                }
                let result = Command::new(cmd.clone())
                    .env_clear()
                    .envs(env::vars().filter(|(k, _)| k == "PATH"))
                    .envs(self.env.clone())
                    .current_dir(&self.cwd)
                    .args(&assertion.args)
                    .with_assert(assert.clone())
                    .output();

                match result {
                    Ok(output) => {
                        let assert = OutputAssert::new(output).with_assert(assert.clone());
                        match assertion.exit {
                            ExitStatus::Success => {
                                assert.stdout_matches(&assertion.expected).success();
                            }
                            ExitStatus::Failure => {
                                assert.stdout_matches(&assertion.expected).failure();
                            }
                        }
                    }
                    Err(err) => {
                        if err.kind() == io::ErrorKind::NotFound {
                            log::error!(target: "test", "{path}: Command `{}` does not exist..", cmd.display());
                        }
                        return Err(io::Error::new(
                            err.kind(),
                            format!("{path}: {err}: `{}`", cmd.display()),
                        ));
                    }
                }
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;

    #[test]
    fn test_parse() {
        let input = r#"
Let's try to track @dave and @sean:
```
$ rad track @dave
Tracking relationship established for @dave.
Nothing to do.

$ rad track @sean
Tracking relationship established for @sean.
Nothing to do.
```
Super, now let's move on to the next step.
```
$ rad sync
```
"#
        .trim()
        .as_bytes()
        .to_owned();

        let mut actual = TestFormula::new();
        let path = Path::new("test.md").to_path_buf();
        actual
            .read(path.as_path(), io::BufReader::new(io::Cursor::new(input)))
            .unwrap();

        let expected = TestFormula {
            cwd: PathBuf::new(),
            env: HashMap::new(),
            subs: Substitutions::new(),
            tests: vec![
                Test {
                    context: vec![String::from("Let's try to track @dave and @sean:")],
                    assertions: vec![
                        Assertion {
                            path: path.clone(),
                            command: String::from("rad"),
                            args: vec![String::from("track"), String::from("@dave")],
                            expected: String::from(
                                "Tracking relationship established for @dave.\nNothing to do.\n\n",
                            ),
                            exit: ExitStatus::Success,
                        },
                        Assertion {
                            path: path.clone(),
                            command: String::from("rad"),
                            args: vec![String::from("track"), String::from("@sean")],
                            expected: String::from(
                                "Tracking relationship established for @sean.\nNothing to do.\n",
                            ),
                            exit: ExitStatus::Success,
                        },
                    ],
                },
                Test {
                    context: vec![String::from("Super, now let's move on to the next step.")],
                    assertions: vec![Assertion {
                        path: path.clone(),
                        command: String::from("rad"),
                        args: vec![String::from("sync")],
                        expected: String::new(),
                        exit: ExitStatus::Success,
                    }],
                },
            ],
        };

        assert_eq!(actual, expected);
    }

    #[test]
    fn test_run() {
        let input = r#"
Running a simple command such as `head`:
```
$ head -n 2 Cargo.toml
[package]
name = "radicle-cli-test"
```
"#
        .trim()
        .as_bytes()
        .to_owned();

        let mut formula = TestFormula::new();
        formula
            .cwd(env!("CARGO_MANIFEST_DIR"))
            .read(
                Path::new("test.md"),
                io::BufReader::new(io::Cursor::new(input)),
            )
            .unwrap();
        formula.run().unwrap();
    }
}
