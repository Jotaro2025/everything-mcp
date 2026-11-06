//! Preserve explicit host targeting when reading or forwarding core settings.

use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Default, Debug, PartialEq)]
pub(crate) struct HostContext {
    pub instance: Option<OsString>,
    pub config: Option<PathBuf>,
}

impl HostContext {
    pub fn current() -> Self {
        Self::parse(std::env::args_os().skip(1))
    }

    fn parse(args: impl IntoIterator<Item = OsString>) -> Self {
        let mut context = Self::default();
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            if arg
                .to_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("-instance"))
            {
                context.instance = args.next();
            } else if arg
                .to_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("-config"))
            {
                context.config = args.next().map(PathBuf::from);
            }
        }
        context
    }

    pub fn target_command(&self, command: &mut std::process::Command) {
        if let Some(instance) = &self.instance {
            command.arg("-instance").arg(instance);
        }
        if let Some(config) = &self.config {
            command.arg("-config").arg(config);
        }
    }

    pub fn ini_name(&self, stem: &str) -> OsString {
        let mut name = OsString::from(stem);
        if let Some(instance) = &self.instance {
            name.push("-");
            name.push(instance);
        }
        name.push(".ini");
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_explicit_host_target_with_spaces_and_unicode() {
        let context = HostContext::parse(
            [
                "-startup",
                "-INSTANCE",
                "test 实例",
                "-config",
                "C:\\test dir\\Everything.ini",
            ]
            .map(OsString::from),
        );
        let mut command = std::process::Command::new("Everything.exe");
        assert_eq!(context.ini_name("Plugins"), "Plugins-test 实例.ini");
        context.target_command(&mut command);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "-instance",
                "test 实例",
                "-config",
                "C:\\test dir\\Everything.ini",
            ]
        );
        assert_eq!(
            HostContext::parse([OsString::from("-config")]),
            HostContext::default()
        );
    }
}
