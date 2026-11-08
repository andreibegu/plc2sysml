use indicatif::{ProgressBar, ProgressStyle};

pub(super) struct Progress(ProgressBar);

impl Progress {
    pub(super) fn new(total: usize) -> Self {
        let bar = ProgressBar::new(total as u64);

        bar.set_style(
            ProgressStyle::with_template("[{bar:40}] {pos}/{len}  {msg}")
                .expect("progress template")
                .progress_chars("=  "),
        );

        Self(bar)
    }

    pub(super) fn show(&self, completed: usize, label: &str) {
        self.0.set_message(label.to_owned());
        self.0.set_position(completed as u64);
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.0.finish_and_clear();
    }
}
