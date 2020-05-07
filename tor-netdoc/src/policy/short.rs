use std::fmt::Display;
use std::str::FromStr;

use super::{PolicyError, PortRange};

#[derive(Clone)]
pub struct PortPolicy {
    allowed: Vec<PortRange>,
}

impl Display for PortPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.allowed.is_empty() {
            write!(f, "reject 1-65535")?;
        } else {
            write!(f, "accept ")?;
            let mut comma = "";
            for range in self.allowed.iter() {
                write!(f, "{}{}", comma, range)?;
                comma = ",";
            }
        }
        Ok(())
    }
}

impl PortPolicy {
    fn invert(&mut self) {
        let mut prev_hi = 0;
        let mut new_allowed = Vec::new();
        for entry in self.allowed.iter() {
            // ports prev_hi+1 through entry.lo-1 were rejected.  We should
            // make them allowed.
            if entry.lo > prev_hi + 1 {
                new_allowed.push(PortRange::new_unchecked(prev_hi + 1, entry.lo - 1));
            }
            prev_hi = entry.hi;
        }
        if prev_hi < 65535 {
            new_allowed.push(PortRange::new_unchecked(prev_hi + 1, 65535));
        }
        self.allowed = new_allowed;
    }
    pub fn allows_port(&self, port: u16) -> bool {
        // TODO: A binary search would be more efficient.
        self.allowed.iter().any(|range| range.contains(port))
    }
}
impl FromStr for PortPolicy {
    type Err = PolicyError;
    fn from_str(mut s: &str) -> Result<Self, PolicyError> {
        let invert = if s.starts_with("accept ") {
            false
        } else if s.starts_with("reject ") {
            true
        } else {
            return Err(PolicyError::InvalidPolicy);
        };
        let mut result = PortPolicy {
            allowed: Vec::new(),
        };
        s = &s[7..];
        for item in s.split(',') {
            let r: PortRange = item.parse()?;
            if let Some(prev) = result.allowed.last() {
                if r.lo <= prev.hi {
                    // Or should this be "<"? TODO XXXX
                    return Err(PolicyError::InvalidPolicy);
                }
            }
            result.allowed.push(r);
        }
        if invert {
            result.invert();
        }
        Ok(result)
    }
}
