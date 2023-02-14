use std::time::Instant;

pub enum Timer {
    Set(Instant),
    Unset,
}

impl Timer {
    pub fn ready(&mut self) -> bool {
        let mut instant = Instant::now();
        match self {
            Timer::Set(time) => time <= &mut instant,
            Timer::Unset => false,
        }
    }
}
