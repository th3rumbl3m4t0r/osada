//! When a job runs (Round 18b): cron's five fields, `minute hour day-of-month month day-of-week`,
//! each `*`, a number, a range `a-b`, a list `a,b,c` and steps `*/n`, `a-b/n`; day-of-week 0-7
//! (0 and 7 Sunday). As cron: when both days are restricted, either matches. In the node's time
//! (localtime_r, its TZ).

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cron {
    minute: Vec<bool>,
    hour: Vec<bool>,
    dom: Vec<bool>,
    month: Vec<bool>,
    dow: Vec<bool>,
    dom_any: bool,
    dow_any: bool,
}

fn field(s: &str, lo: u32, hi: u32, what: &str) -> Result<(Vec<bool>, bool), String> {
    let mut on = vec![false; hi as usize + 1];
    let any = s == "*";
    for part in s.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, st)) => (r, st.parse::<u32>().ok().filter(|n| *n > 0).ok_or_else(|| format!("{what}: {st:?} is not a step"))?),
            None => (part, 1),
        };
        let (a, b) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            let a = a.parse::<u32>().map_err(|_| format!("{what}: {a:?} is not a number"))?;
            let b = b.parse::<u32>().map_err(|_| format!("{what}: {b:?} is not a number"))?;
            (a, b)
        } else {
            let a = range.parse::<u32>().map_err(|_| format!("{what}: {range:?} is not a number"))?;
            // `5/15`: from 5 to the end, every 15
            (a, if part.contains('/') { hi } else { a })
        };
        if a < lo || b > hi || a > b {
            return Err(format!("{what}: {part:?} is not within {lo}-{hi}"));
        }
        let mut x = a;
        while x <= b {
            on[x as usize] = true;
            x += step;
        }
    }
    Ok((on, any))
}

impl Cron {
    pub fn parse(s: &str) -> Result<Cron, String> {
        let f: Vec<&str> = s.split_whitespace().collect();
        if f.len() != 5 {
            return Err(format!("{s:?}: five fields, minute hour day month weekday (e.g. 4,24,44 * * * *)"));
        }
        let (minute, _) = field(f[0], 0, 59, "minute")?;
        let (hour, _) = field(f[1], 0, 23, "hour")?;
        let (dom, dom_any) = field(f[2], 1, 31, "day of the month")?;
        let (month, _) = field(f[3], 1, 12, "month")?;
        let (mut dow, dow_any) = field(f[4], 0, 7, "day of the week")?;
        if dow[7] {
            dow[0] = true;
        }
        Ok(Cron { minute, hour, dom, month, dow, dom_any, dow_any })
    }

    /// Whether it runs in this minute (`dow`: 0 Sunday).
    pub fn matches(&self, minute: u32, hour: u32, dom: u32, month: u32, dow: u32) -> bool {
        let day = match (self.dom_any, self.dow_any) {
            (true, true) => true,
            (false, true) => self.dom[dom as usize],
            (true, false) => self.dow[dow as usize],
            (false, false) => self.dom[dom as usize] || self.dow[dow as usize],
        };
        self.minute[minute as usize] && self.hour[hour as usize] && self.month[month as usize] && day
    }

    /// Whether it runs in the minute of `ts` (unix seconds), the node's time.
    pub fn matches_at(&self, ts: i64) -> bool {
        let t = local(ts);
        self.matches(t.tm_min as u32, t.tm_hour as u32, t.tm_mday as u32, t.tm_mon as u32 + 1, t.tm_wday as u32)
    }

    /// The next minute it runs after `ts` (within a year): its unix seconds.
    pub fn next_after(&self, ts: i64) -> Option<i64> {
        let mut x = ts - ts.rem_euclid(60) + 60;
        for _ in 0..(366 * 24 * 60) {
            if self.matches_at(x) {
                return Some(x);
            }
            x += 60;
        }
        None
    }
}

/// The node's time of `ts` (localtime_r).
pub fn local(ts: i64) -> libc::tm {
    let t = ts as libc::time_t;
    // SAFETY: localtime_r writes into the tm given, reads t
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields() {
        let c = Cron::parse("4,24,44 * * * *").unwrap();
        assert!(c.matches(4, 0, 1, 1, 0) && c.matches(44, 23, 31, 12, 6));
        assert!(!c.matches(5, 0, 1, 1, 0));
        let c = Cron::parse("*/15 8-17 * * 1-5").unwrap();
        assert!(c.matches(30, 9, 18, 11, 3) && !c.matches(30, 9, 21, 11, 6) && !c.matches(31, 9, 18, 11, 3) && !c.matches(0, 18, 18, 11, 3));
        let c = Cron::parse("0 3 1 * 7").unwrap();
        assert!(c.matches(0, 3, 1, 5, 2), "the 1st, any weekday");
        assert!(c.matches(0, 3, 9, 5, 0), "a Sunday (7), any day");
        assert!(!c.matches(0, 3, 9, 5, 2));
        let c = Cron::parse("5/20 * * * *").unwrap();
        assert!(c.matches(5, 0, 1, 1, 0) && c.matches(25, 0, 1, 1, 0) && c.matches(45, 0, 1, 1, 0) && !c.matches(0, 0, 1, 1, 0));
        for bad in ["* * * *", "60 * * * *", "* 24 * * *", "*/0 * * * *", "a * * * *", "5-1 * * * *", "* * 0 * *"] {
            assert!(Cron::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_next_run() {
        let c = Cron::parse("4,24,44 * * * *").unwrap();
        // whatever the zone, minutes are minutes (zones are whole quarter hours)
        let t = 1_794_990_270; // a :24:30
        let tm = local(t);
        assert_eq!(tm.tm_min, 24);
        assert_eq!(c.next_after(t), Some(t - 30 + 20 * 60));
    }
}
