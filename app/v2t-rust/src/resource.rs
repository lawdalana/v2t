#[derive(Clone, Copy, Debug, Default)]
pub struct Snapshot {
    pub ram: Option<u64>,
    pub vram: Option<u64>,
    pub gpu: Option<u32>,
    pub game: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reason {
    #[default]
    None,
    Game,
    LowRam,
    LowVram,
    BusyGpu,
}

impl Reason {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "Local ready",
            Self::Game => "game detected",
            Self::LowRam => "low RAM",
            Self::LowVram => "low VRAM",
            Self::BusyGpu => "GPU busy",
        }
    }
}

#[derive(Default)]
pub struct Routing {
    pub current: Reason,
    busy: u8,
    healthy: u8,
}

impl Routing {
    pub fn update(&mut self, sample: Snapshot) -> Reason {
        const GIB: u64 = 1024 * 1024 * 1024;
        let gpu_busy = sample.gpu.is_some_and(|value| value >= 50);
        self.busy = if gpu_busy { (self.busy + 1).min(2) } else { 0 };
        let reason = if sample.game {
            Reason::Game
        } else if sample.ram.is_some_and(|bytes| bytes < 2 * GIB) {
            Reason::LowRam
        } else if sample.vram.is_some_and(|bytes| bytes < 3 * GIB / 2) {
            Reason::LowVram
        } else if self.busy >= 2 {
            Reason::BusyGpu
        } else {
            Reason::None
        };
        if reason != Reason::None {
            self.current = reason;
            self.healthy = 0;
        } else if self.current != Reason::None {
            let healthy = !gpu_busy
                && sample.ram.is_none_or(|bytes| bytes >= 3 * GIB)
                && sample.vram.is_none_or(|bytes| bytes >= 2 * GIB);
            self.healthy = if healthy { self.healthy + 1 } else { 0 };
            if self.healthy >= 3 {
                *self = Self::default();
            }
        }
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pauses_resources_and_requires_three_healthy_samples() {
        let mut routing = Routing::default();
        let busy = Snapshot {
            gpu: Some(50),
            ..Snapshot::default()
        };
        assert_eq!(routing.update(busy), Reason::None);
        assert_eq!(routing.update(busy), Reason::BusyGpu);
        assert_eq!(routing.update(Snapshot::default()), Reason::BusyGpu);
        let low = Snapshot {
            ram: Some(1),
            ..Snapshot::default()
        };
        assert_eq!(routing.update(low), Reason::LowRam);
        for _ in 0..2 {
            assert_eq!(routing.update(Snapshot::default()), Reason::LowRam);
        }
        assert_eq!(routing.update(Snapshot::default()), Reason::None);
    }
}
