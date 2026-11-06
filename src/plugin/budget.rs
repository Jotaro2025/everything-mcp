//! Cooperative request budgets. Every successful OS read is charged, including
//! bytes later rejected as binary or malformed. Blocking filesystem calls may
//! still be delayed by Windows/network drivers; checks run between operations.

use std::io::{self, Read};
use std::time::{Duration, Instant};

pub(crate) struct ScanBudget {
    pub bytes: u64,
    limit: u64,
    deadline: Instant,
    pub stop_reason: Option<&'static str>,
}

impl ScanBudget {
    pub fn new(limit: u64, duration: Duration) -> Self {
        Self::until(limit, Instant::now() + duration)
    }

    pub fn until(limit: u64, deadline: Instant) -> Self {
        Self {
            bytes: 0,
            limit,
            deadline,
            stop_reason: None,
        }
    }

    pub fn check(&mut self) -> bool {
        if self.stop_reason.is_none() {
            self.stop_reason = if crate::mcp::server::request_cancelled() {
                Some("cancelled")
            } else if Instant::now() >= self.deadline {
                Some("deadline")
            } else {
                None
            };
        }
        self.stop_reason.is_none()
    }

    pub fn read(&mut self, reader: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
        if !self.check() {
            return Err(io::Error::other(self.stop_reason.unwrap_or("cancelled")));
        }
        let capacity = (self.limit - self.bytes).min(buf.len() as u64) as usize;
        if capacity == 0 && !buf.is_empty() {
            self.stop_reason = Some("byte_budget");
            return Err(io::Error::other("scan byte budget exhausted"));
        }
        let n = reader.read(&mut buf[..capacity])?;
        self.bytes += n as u64;
        Ok(n)
    }
}
