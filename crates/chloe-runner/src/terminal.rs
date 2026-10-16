//! ホスト側の端末の扱い。

use anyhow::Result;
use chloe_agent_proto::WindowSize;
use rustix::termios::{self, OptionalActions, Termios};

/// 端末の大きさ。端末でなければ 24x80 とみなす。
pub fn size() -> WindowSize {
    match termios::tcgetwinsize(std::io::stdout()) {
        Ok(size) if size.ws_row > 0 && size.ws_col > 0 => WindowSize {
            rows: size.ws_row,
            cols: size.ws_col,
        },
        _ => WindowSize { rows: 24, cols: 80 },
    }
}

/// 標準入力の端末を raw モードにし、drop で元に戻す。
pub struct RawMode {
    original: Termios,
}

impl RawMode {
    /// 標準入力が端末でなければ何もせず `None` を返す。
    pub fn enable() -> Result<Option<Self>> {
        let stdin = std::io::stdin();
        if !termios::isatty(&stdin) {
            return Ok(None);
        }
        let original = termios::tcgetattr(&stdin)?;
        let mut raw = original.clone();
        raw.make_raw();
        termios::tcsetattr(&stdin, OptionalActions::Now, &raw)?;
        Ok(Some(Self { original }))
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = termios::tcsetattr(std::io::stdin(), OptionalActions::Now, &self.original);
    }
}
