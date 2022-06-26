pub struct Logger {
    pub log_level: String
}

impl Logger {
    pub fn new(log_level: String) -> Logger {
        Logger {
            log_level
        }
    }
    pub fn log(&self, message: &str) {
        if self.log_level != "none" {
            println!("{}", message);
        }
    }
}
