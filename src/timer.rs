//! 番茄钟状态机。
//!
//! 和 Python 版那个番茄钟保持同一套规则：专注 25 分钟、短休息 5 分钟、
//! 每完成 4 轮专注换成 15 分钟长休息。

/// 专注时长（秒）
pub const FOCUS_SECS: f32 = 25.0 * 60.0;
/// 短休息（秒）
pub const SHORT_SECS: f32 = 5.0 * 60.0;
/// 长休息（秒）
pub const LONG_SECS: f32 = 15.0 * 60.0;
/// 几轮专注之后来一次长休息
pub const CYCLES_PER_LONG: u32 = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Focus,
    Short,
    Long,
}

impl Phase {
    pub fn total(self) -> f32 {
        match self {
            Phase::Focus => FOCUS_SECS,
            Phase::Short => SHORT_SECS,
            Phase::Long => LONG_SECS,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Phase::Focus => "专注",
            Phase::Short => "短休息",
            Phase::Long => "长休息",
        }
    }

    pub fn is_break(self) -> bool {
        !matches!(self, Phase::Focus)
    }
}

pub struct Pomodoro {
    pub phase: Phase,
    /// 本阶段还剩多少秒
    pub remaining: f32,
    pub running: bool,
    /// 已经完成了几轮专注
    pub done: u32,
    /// 这一帧刚切换阶段（用来响铃）
    pub chime: bool,
}

impl Default for Pomodoro {
    fn default() -> Self {
        Self::new()
    }
}

impl Pomodoro {
    pub fn new() -> Self {
        Self {
            phase: Phase::Focus,
            remaining: Phase::Focus.total(),
            running: false,
            done: 0,
            chime: false,
        }
    }

    /// 每秒推进。`dt` 是上一帧到这一帧的秒数。
    pub fn tick(&mut self, dt: f32) {
        self.chime = false;
        if !self.running {
            return;
        }
        self.remaining -= dt;
        // 超出的时间要带到下一阶段，不能直接丢掉；
        // guard 防止极端 dt（比如系统休眠几小时）把这里变成死循环。
        let mut guard = 0;
        while self.remaining <= 0.0 && guard < 8 {
            let over = -self.remaining;
            self.advance();
            self.remaining -= over;
            guard += 1;
        }
        if self.remaining < 0.0 {
            // 极端情况：停在阶段开头，下一帧继续推进
            self.remaining = 0.0;
        }
    }

    /// 立刻进入下一阶段
    pub fn advance(&mut self) {
        if self.phase == Phase::Focus {
            self.done += 1;
            self.phase = if self.done % CYCLES_PER_LONG == 0 {
                Phase::Long
            } else {
                Phase::Short
            };
        } else {
            self.phase = Phase::Focus;
        }
        self.remaining = self.phase.total();
        self.chime = true;
    }

    pub fn toggle(&mut self) {
        self.running = !self.running;
    }

    /// 回到本阶段开头并暂停
    pub fn reset(&mut self) {
        self.remaining = self.phase.total();
        self.running = false;
    }

    /// 全部归零，从第一个番茄重新开始
    pub fn reset_all(&mut self) {
        *self = Self::new();
    }

    /// "25:00" 这样的显示文本。用 ceil 保证最后 1 秒才显示 00:00。
    pub fn text(&self) -> String {
        let s = self.remaining.max(0.0).ceil() as u32;
        format!("{:02}:{:02}", s / 60, s % 60)
    }

    /// 本阶段已完成的比例，0.0 ~ 1.0
    pub fn progress(&self) -> f32 {
        (1.0 - self.remaining / self.phase.total()).clamp(0.0, 1.0)
    }

    /// 副标题：轮次 + 状态
    pub fn subtitle(&self) -> String {
        let mut s = if self.phase.is_break() {
            format!("{} · 已完成 {} 轮", self.phase.label(), self.done)
        } else {
            let n = self.done % CYCLES_PER_LONG + 1;
            format!("{} · 第 {}/{} 轮", self.phase.label(), n, CYCLES_PER_LONG)
        };
        if !self.running {
            s.push_str(" · 已暂停");
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_paused_at_25_minutes() {
        let p = Pomodoro::new();
        assert_eq!(p.text(), "25:00");
        assert!(!p.running);
        assert_eq!(p.phase, Phase::Focus);
    }

    #[test]
    fn counts_down_only_when_running() {
        let mut p = Pomodoro::new();
        p.tick(1.0);
        assert_eq!(p.text(), "25:00");
        p.toggle();
        p.tick(1.0);
        assert_eq!(p.text(), "24:59");
    }

    #[test]
    fn four_focus_rounds_then_long_break() {
        let mut p = Pomodoro::new();
        p.running = true;
        p.remaining = 0.05;
        p.tick(0.1);
        assert_eq!(p.phase, Phase::Short);
        assert_eq!(p.done, 1);

        // 第 2、3 轮之后还是短休息
        for expect_done in 2..=3 {
            p.advance(); // 回到专注
            assert_eq!(p.phase, Phase::Focus);
            p.advance(); // 专注结束
            assert_eq!(p.phase, Phase::Short);
            assert_eq!(p.done, expect_done);
        }

        p.advance(); // 第 4 轮专注开始
        assert_eq!(p.phase, Phase::Focus);
        p.advance(); // 第 4 轮结束 → 长休息
        assert_eq!(p.phase, Phase::Long);
        assert_eq!(p.done, 4);
    }

    #[test]
    fn reset_rewinds_and_pauses() {
        let mut p = Pomodoro::new();
        p.toggle();
        p.tick(30.0);
        assert_ne!(p.text(), "25:00");
        p.reset();
        assert_eq!(p.text(), "25:00");
        assert!(!p.running);
    }

    #[test]
    fn progress_goes_0_to_1() {
        let mut p = Pomodoro::new();
        assert!(p.progress() < 0.01);
        p.toggle();
        p.tick(FOCUS_SECS / 2.0);
        assert!((p.progress() - 0.5).abs() < 0.01);
    }

    #[test]
    fn display_never_goes_negative() {
        let mut p = Pomodoro::new();
        p.toggle();
        // 超大 dt（比如系统休眠了一夜）也不能出现负数、不能死循环
        p.tick(10_000.0);
        let t = p.text();
        assert!(!t.starts_with('-'), "显示不能是负数: {t}");
        assert_eq!(t.len(), 5, "必须是 mm:ss: {t}");
        assert!((0.0..=1.0).contains(&p.progress()), "进度必须在 0~1");
    }
}
