use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use rodio::{Source, source::SeekError};

pub(crate) struct PlaybackRangeSource<S> {
    input: S,
    start: Duration,
    end: Option<Duration>,
    remaining: Option<Duration>,
    duration_per_sample: Duration,
    looping: Arc<AtomicBool>,
}

impl<S> PlaybackRangeSource<S>
where
    S: Source,
{
    pub(crate) fn new(
        input: S,
        start: Duration,
        end: Option<Duration>,
        position: Duration,
        looping: Arc<AtomicBool>,
    ) -> Self {
        let duration_per_sample = Self::duration_per_sample(&input);

        Self {
            input,
            start,
            end,
            remaining: end.map(|end| end.saturating_sub(position)),
            duration_per_sample,
            looping,
        }
    }

    fn duration_per_sample(input: &S) -> Duration {
        Duration::from_secs_f64(
            1.0 / (f64::from(input.sample_rate().get()) * f64::from(input.channels().get())),
        )
    }

    fn restart(&mut self) -> bool {
        if !self.looping.load(Ordering::Relaxed) || self.input.try_seek(self.start).is_err() {
            return false;
        }

        self.remaining = self.end.map(|end| end.saturating_sub(self.start));
        self.duration_per_sample = Self::duration_per_sample(&self.input);
        true
    }
}

impl<S> Iterator for PlaybackRangeSource<S>
where
    S: Source,
{
    type Item = S::Item;

    fn next(&mut self) -> Option<Self::Item> {
        if self
            .remaining
            .is_some_and(|remaining| remaining <= self.duration_per_sample)
            && !self.restart()
        {
            return None;
        }

        if let Some(sample) = self.input.next() {
            if let Some(remaining) = self.remaining.as_mut() {
                *remaining = remaining.saturating_sub(self.duration_per_sample);
            }

            return Some(sample);
        }

        if self.restart() {
            let sample = self.input.next()?;

            if let Some(remaining) = self.remaining.as_mut() {
                *remaining = remaining.saturating_sub(self.duration_per_sample);
            }

            Some(sample)
        } else {
            None
        }
    }
}

impl<S> Source for PlaybackRangeSource<S>
where
    S: Source,
{
    fn current_span_len(&self) -> Option<usize> {
        let remaining_samples = self.remaining.map(|remaining| {
            let samples = remaining.as_nanos() / self.duration_per_sample.as_nanos();
            let channels = u128::from(self.input.channels().get());
            (samples - samples % channels) as usize
        });

        let span = match (self.input.current_span_len(), remaining_samples) {
            (Some(input), Some(remaining)) => Some(input.min(remaining)),
            (Some(input), None) => Some(input),
            (None, remaining) => remaining,
        };

        // Don't report an infinite span. Downstream sources only re-read
        // sample rate at span boundaries.
        if span.is_none() {
            let channels = usize::from(self.input.channels().get());
            const FALLBACK_SAMPLES: usize = 512;
            return Some(FALLBACK_SAMPLES.div_ceil(channels) * channels);
        }

        span
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.input.channels()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.input.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        if self.looping.load(Ordering::Relaxed) {
            None
        } else {
            self.end.map(|end| end.saturating_sub(self.start))
        }
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        let position = self.end.map_or(position.max(self.start), |end| {
            position.clamp(self.start, end)
        });
        self.input.try_seek(position)?;
        self.remaining = self.end.map(|end| end.saturating_sub(position));
        self.duration_per_sample = Self::duration_per_sample(&self.input);
        Ok(())
    }
}
