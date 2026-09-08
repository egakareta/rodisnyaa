use crate::nyaa::{
    AudioAsset, AudioEffects, AudioOutputError, Backend, Device, Nyaa, NyaaError, Output,
};
use std::collections::HashSet;
use std::ops::Range;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::time::Duration;

/// A set of audio players with shared output, configuration, and transport controls.
///
/// A group owns the individual [`Nyaa`] players required to mix sounds concurrently. Sources are
/// loaded before any player starts, then released for playback together so decoding one source
/// does not make it start early.
///
/// ```no_run
/// use rodisnyaa::{NyaaError, NyaaGroup};
///
/// fn play_layers(layers: [&'static [u8]; 2]) -> Result<(), NyaaError> {
///     let mut group = NyaaGroup::new();
///     group.set_volume(0.8);
///     group.try_set_speed(0.9)?;
///     group.load_static_bytes(layers)?;
///     group.play()?;
///     Ok(())
/// }
/// ```
///
/// Named members can be controlled independently:
///
/// ```no_run
/// use rodisnyaa::{NyaaError, NyaaGroup};
///
/// fn play_selected(music: &'static [u8], ambience: &'static [u8]) -> Result<(), NyaaError> {
///     let mut group = NyaaGroup::new();
///     group.add_static_bytes("music", music)?;
///     group.add_static_bytes("ambience", ambience)?;
///     group.member_mut("ambience").unwrap().set_volume(0.2);
///     group.play_member("music")?;
///     Ok(())
/// }
/// ```
pub struct NyaaGroup {
    output: Output,
    preferred_backend: Option<Backend>,
    members: Vec<Nyaa>,
    member_keys: Vec<Option<String>>,
    volume: f32,
    speed: f32,
    preserve_pitch: bool,
    effects: AudioEffects,
    looping: bool,
    loop_range: Option<Range<Duration>>,
}

impl Default for NyaaGroup {
    fn default() -> Self {
        Self::new()
    }
}

impl NyaaGroup {
    /// Creates an empty group with a shared default audio output.
    pub fn new() -> Self {
        let output = Output::new();

        Self::from_output(output, None)
    }

    /// Creates an empty group connected to a reusable audio output.
    pub fn new_with_output(output: Output) -> Self {
        let preferred_backend = output.backend();

        Self::from_output(output, preferred_backend)
    }

    fn from_output(output: Output, preferred_backend: Option<Backend>) -> Self {
        Self {
            output,
            preferred_backend,
            members: Vec::new(),
            member_keys: Vec::new(),
            volume: 1.0,
            speed: 1.0,
            preserve_pitch: false,
            effects: AudioEffects::default(),
            looping: false,
            loop_range: None,
        }
    }

    fn configured_member(&self) -> Result<Nyaa, NyaaError> {
        let mut nyaa = Nyaa::new_with_output(self.output.clone());

        nyaa.set_volume(self.volume);
        nyaa.try_set_speed(self.speed)?;
        nyaa.set_preserve_pitch(self.preserve_pitch)?;
        nyaa.set_effects(self.effects)?;
        nyaa.set_looping(self.looping);
        if let Some(range) = self.loop_range.clone() {
            nyaa.set_loop_range(range)?;
        }

        Ok(nyaa)
    }

    fn replace_members<I, T, F>(&mut self, sources: I, mut load: F) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = T>,
        F: FnMut(&mut Nyaa, T) -> Result<(), NyaaError>,
    {
        let mut members = Vec::new();

        for source in sources {
            let mut nyaa = self.configured_member()?;
            load(&mut nyaa, source)?;
            members.push(nyaa);
        }

        self.members = members;
        self.member_keys = vec![None; self.members.len()];
        Ok(())
    }

    fn key_from_value(key: impl Into<String>) -> Result<String, NyaaError> {
        let key = key.into();
        if key.trim().is_empty() {
            return Err(NyaaError::InvalidGroupKey);
        }

        Ok(key)
    }

    fn has_key(&self, key: &str) -> bool {
        self.member_keys
            .iter()
            .any(|member_key| member_key.as_deref() == Some(key))
    }

    fn member_index(&self, key: &str) -> Result<usize, NyaaError> {
        self.member_keys
            .iter()
            .position(|member_key| member_key.as_deref() == Some(key))
            .ok_or_else(|| NyaaError::UnknownGroupKey(key.to_string()))
    }

    fn add_member<K, F>(&mut self, key: K, load: F) -> Result<(), NyaaError>
    where
        K: Into<String>,
        F: FnOnce(&mut Nyaa) -> Result<(), NyaaError>,
    {
        let key = Self::key_from_value(key)?;
        if self.has_key(&key) {
            return Err(NyaaError::DuplicateGroupKey(key));
        }

        let mut nyaa = self.configured_member()?;
        load(&mut nyaa)?;
        self.members.push(nyaa);
        self.member_keys.push(Some(key));
        Ok(())
    }

    fn replace_keyed_members<I, K, T, F>(
        &mut self,
        sources: I,
        mut load: F,
    ) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = (K, T)>,
        K: Into<String>,
        F: FnMut(&mut Nyaa, T) -> Result<(), NyaaError>,
    {
        let mut keyed_sources = Vec::new();
        let mut keys = HashSet::new();

        for (key, source) in sources {
            let key = Self::key_from_value(key)?;
            if !keys.insert(key.clone()) {
                return Err(NyaaError::DuplicateGroupKey(key));
            }
            keyed_sources.push((key, source));
        }

        let mut members = Vec::new();
        let mut member_keys = Vec::new();
        for (key, source) in keyed_sources {
            let mut member = self.configured_member()?;
            load(&mut member, source)?;
            members.push(member);
            member_keys.push(Some(key));
        }

        self.member_keys = member_keys;
        self.members = members;
        Ok(())
    }

    /// Loads static encoded audio sources without starting playback.
    ///
    /// Any sources currently owned by the group are replaced only after every new source loads
    /// successfully.
    pub fn load_static_bytes<I>(&mut self, sources: I) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = &'static [u8]>,
    {
        self.replace_members(sources, Nyaa::load_static_bytes)
    }

    /// Loads named static encoded audio sources without starting playback.
    pub fn load_static_bytes_keyed<I, K>(&mut self, sources: I) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = (K, &'static [u8])>,
        K: Into<String>,
    {
        self.replace_keyed_members(sources, Nyaa::load_static_bytes)
    }

    /// Loads one named static encoded audio source without starting playback.
    pub fn add_static_bytes(
        &mut self,
        key: impl Into<String>,
        bytes: &'static [u8],
    ) -> Result<(), NyaaError> {
        self.add_member(key, |nyaa| nyaa.load_static_bytes(bytes))
    }

    /// Loads static encoded audio sources and starts them together.
    pub fn play_static_bytes<I>(&mut self, sources: I) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = &'static [u8]>,
    {
        self.load_static_bytes(sources)?;
        self.play()
    }

    /// Loads named static encoded audio sources and starts them together.
    pub fn play_static_bytes_keyed<I, K>(&mut self, sources: I) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = (K, &'static [u8])>,
        K: Into<String>,
    {
        self.load_static_bytes_keyed(sources)?;
        self.play()
    }

    /// Copies encoded audio sources into shared storage without starting playback.
    ///
    /// Any sources currently owned by the group are replaced only after every new source loads
    /// successfully.
    pub fn load_shared_bytes<I, B>(&mut self, sources: I) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        self.replace_members(sources, |nyaa, bytes| nyaa.load_shared_bytes(bytes))
    }

    /// Loads named encoded audio sources into shared storage without starting playback.
    pub fn load_shared_bytes_keyed<I, K, B>(&mut self, sources: I) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = (K, B)>,
        K: Into<String>,
        B: AsRef<[u8]>,
    {
        self.replace_keyed_members(sources, |nyaa, bytes| nyaa.load_shared_bytes(bytes))
    }

    /// Loads one named encoded audio source into shared storage without starting playback.
    pub fn add_shared_bytes<B>(&mut self, key: impl Into<String>, bytes: B) -> Result<(), NyaaError>
    where
        B: AsRef<[u8]>,
    {
        self.add_member(key, |nyaa| nyaa.load_shared_bytes(bytes))
    }

    /// Copies encoded audio sources into shared storage and starts them together.
    pub fn play_shared_bytes<I, B>(&mut self, sources: I) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        self.load_shared_bytes(sources)?;
        self.play()
    }

    /// Loads named encoded audio sources into shared storage and starts them together.
    pub fn play_shared_bytes_keyed<I, K, B>(&mut self, sources: I) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = (K, B)>,
        K: Into<String>,
        B: AsRef<[u8]>,
    {
        self.load_shared_bytes_keyed(sources)?;
        self.play()
    }

    /// Loads native audio files without starting playback.
    ///
    /// Any sources currently owned by the group are replaced only after every new source loads
    /// successfully.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_files<I, P>(&mut self, paths: I) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        self.replace_members(paths, |nyaa, path| nyaa.load_file(path))
    }

    /// Loads named native audio files without starting playback.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_files_keyed<I, K, P>(&mut self, paths: I) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = (K, P)>,
        K: Into<String>,
        P: AsRef<Path>,
    {
        self.replace_keyed_members(paths, |nyaa, path| nyaa.load_file(path))
    }

    /// Loads one named native audio file without starting playback.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn add_file<P>(&mut self, key: impl Into<String>, path: P) -> Result<(), NyaaError>
    where
        P: AsRef<Path>,
    {
        self.add_member(key, |nyaa| nyaa.load_file(path))
    }

    /// Loads native audio files and starts them together.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn play_files<I, P>(&mut self, paths: I) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        self.load_files(paths)?;
        self.play()
    }

    /// Loads named native audio files and starts them together.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn play_files_keyed<I, K, P>(&mut self, paths: I) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = (K, P)>,
        K: Into<String>,
        P: AsRef<Path>,
    {
        self.load_files_keyed(paths)?;
        self.play()
    }

    /// Loads cross-platform audio assets without starting playback.
    ///
    /// Browser assets are fetched before the group's sources are replaced.
    pub async fn load_assets<'a, I>(&mut self, assets: I) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = &'a AudioAsset>,
    {
        let mut members = Vec::new();

        for asset in assets {
            let mut nyaa = self.configured_member()?;
            nyaa.load_asset(asset).await?;
            members.push(nyaa);
        }

        self.members = members;
        self.member_keys = vec![None; self.members.len()];
        Ok(())
    }

    /// Loads named cross-platform audio assets without starting playback.
    ///
    /// Browser assets are fetched before the group's sources are replaced.
    pub async fn load_assets_keyed<'a, I, K>(&mut self, assets: I) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = (K, &'a AudioAsset)>,
        K: Into<String>,
    {
        let mut keyed_assets = Vec::new();
        let mut keys = HashSet::new();

        for (key, asset) in assets {
            let key = Self::key_from_value(key)?;
            if !keys.insert(key.clone()) {
                return Err(NyaaError::DuplicateGroupKey(key));
            }
            keyed_assets.push((key, asset));
        }

        let mut members = Vec::new();
        let mut member_keys = Vec::new();
        for (key, asset) in keyed_assets {
            let mut nyaa = self.configured_member()?;
            nyaa.load_asset(asset).await?;
            members.push(nyaa);
            member_keys.push(Some(key));
        }

        self.members = members;
        self.member_keys = member_keys;
        Ok(())
    }

    /// Loads one named cross-platform audio asset without starting playback.
    pub async fn add_asset(
        &mut self,
        key: impl Into<String>,
        asset: &AudioAsset,
    ) -> Result<(), NyaaError> {
        let key = Self::key_from_value(key)?;
        if self.has_key(&key) {
            return Err(NyaaError::DuplicateGroupKey(key));
        }

        let mut nyaa = self.configured_member()?;
        nyaa.load_asset(asset).await?;
        self.members.push(nyaa);
        self.member_keys.push(Some(key));
        Ok(())
    }

    /// Loads cross-platform audio assets and starts them together.
    ///
    /// On browser targets, call this from a user gesture so the shared output can be initialized
    /// before the first network request yields.
    pub async fn play_assets<'a, I>(&mut self, assets: I) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = &'a AudioAsset>,
    {
        #[cfg(target_arch = "wasm32")]
        self.ensure_output()?;

        self.load_assets(assets).await?;
        self.play()
    }

    /// Loads named cross-platform audio assets and starts them together.
    pub async fn play_assets_keyed<'a, I, K>(&mut self, assets: I) -> Result<(), NyaaError>
    where
        I: IntoIterator<Item = (K, &'a AudioAsset)>,
        K: Into<String>,
    {
        #[cfg(target_arch = "wasm32")]
        self.ensure_output()?;

        self.load_assets_keyed(assets).await?;
        self.play()
    }

    /// Returns the keys of all named members in insertion order.
    pub fn member_keys(&self) -> impl Iterator<Item = &str> {
        self.member_keys.iter().filter_map(Option::as_deref)
    }

    /// Returns a named member for read-only inspection.
    pub fn member(&self, key: &str) -> Option<&Nyaa> {
        self.member_index(key)
            .ok()
            .and_then(|index| self.members.get(index))
    }

    /// Returns a named member for individual configuration.
    ///
    /// Changes made through the returned member affect only that member. Group-wide setters called
    /// later can overwrite those individual settings.
    pub fn member_mut(&mut self, key: &str) -> Option<&mut Nyaa> {
        let index = self.member_index(key).ok()?;
        self.members.get_mut(index)
    }

    /// Starts one named member without changing the other members.
    pub fn play_member(&mut self, key: &str) -> Result<(), NyaaError> {
        let index = self.member_index(key)?;
        let nyaa = &mut self.members[index];

        if nyaa.is_playing() {
            return Ok(());
        }
        if nyaa.is_paused() {
            nyaa.resume();
            return Ok(());
        }

        let position = nyaa.position();
        nyaa.prepare_current_source_at(position)?;
        nyaa.start_prepared();
        Ok(())
    }

    /// Pauses one named member without changing the other members.
    pub fn pause_member(&self, key: &str) -> Result<(), NyaaError> {
        let index = self.member_index(key)?;
        self.members[index].pause();
        Ok(())
    }

    /// Resumes one named member without changing the other members.
    pub fn resume_member(&self, key: &str) -> Result<(), NyaaError> {
        let index = self.member_index(key)?;
        self.members[index].resume();
        Ok(())
    }

    /// Stops one named member without changing the other members.
    pub fn stop_member(&mut self, key: &str) -> Result<(), NyaaError> {
        let index = self.member_index(key)?;
        self.members[index].stop();
        Ok(())
    }

    /// Seeks one named member without changing the other members.
    pub fn try_seek_member(&mut self, key: &str, position: Duration) -> Result<(), NyaaError> {
        let index = self.member_index(key)?;
        self.members[index].try_seek(position)
    }

    /// Starts all loaded sources together from their shared group position.
    ///
    /// Calling this while every member is paused resumes them. Otherwise each decoder is prepared
    /// at the position reported by the first member before any member starts.
    pub fn play(&mut self) -> Result<(), NyaaError> {
        if self.members.is_empty() || self.members.iter().all(Nyaa::is_playing) {
            return Ok(());
        }

        if self.members.iter().all(Nyaa::is_paused) {
            self.resume();
            return Ok(());
        }

        let position = self.position();
        for index in 0..self.members.len() {
            if let Err(error) = self.members[index].prepare_current_source_at(position) {
                for nyaa in &mut self.members[..index] {
                    nyaa.cancel_prepared();
                }
                return Err(error);
            }
        }

        for nyaa in &self.members {
            nyaa.start_prepared();
        }

        Ok(())
    }

    /// Pauses every playing source in the group.
    pub fn pause(&self) {
        for nyaa in &self.members {
            nyaa.pause();
        }
    }

    /// Resumes every paused source in the group.
    pub fn resume(&self) {
        for nyaa in &self.members {
            nyaa.resume();
        }
    }

    /// Stops every source in the group.
    ///
    /// Like [`Nyaa::stop`], this removes each current source. Load sources again before calling
    /// [`NyaaGroup::play`].
    pub fn stop(&mut self) {
        for nyaa in &mut self.members {
            nyaa.stop();
        }
    }

    /// Seeks every source to the same source-time position.
    ///
    /// Sources that were playing are paused during the operation and resumed afterward.
    pub fn try_seek(&mut self, position: Duration) -> Result<(), NyaaError> {
        let positions: Vec<Duration> = self.members.iter().map(Nyaa::position).collect();
        let was_playing: Vec<bool> = self.members.iter().map(Nyaa::is_playing).collect();

        self.pause();
        for index in 0..self.members.len() {
            if let Err(error) = self.members[index].try_seek(position) {
                for (nyaa, previous) in self.members.iter_mut().zip(positions) {
                    let _ = nyaa.try_seek(previous);
                }
                for (nyaa, resume) in self.members.iter().zip(was_playing) {
                    if resume {
                        nyaa.resume();
                    }
                }
                return Err(error);
            }
        }

        for (nyaa, resume) in self.members.iter().zip(was_playing) {
            if resume {
                nyaa.resume();
            }
        }
        Ok(())
    }

    /// Convenience version of [`NyaaGroup::try_seek`] that accepts seconds.
    pub fn try_seek_secs(&mut self, position_secs: f64) -> Result<(), NyaaError> {
        self.try_seek(Duration::from_secs_f64(position_secs))
    }

    /// Sets the volume of every current and future member in the group.
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume;
        for nyaa in &self.members {
            nyaa.set_volume(volume);
        }
    }

    /// Returns the volume applied to every member in the group.
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Sets the playback speed of every current and future member in the group.
    ///
    /// Prefer [`NyaaGroup::try_set_speed`] when pitch preservation is enabled so errors are not
    /// discarded.
    pub fn set_speed(&mut self, speed: f32) {
        let _ = self.try_set_speed(speed);
    }

    /// Sets the playback speed of every current and future member and reports rebuild errors.
    pub fn try_set_speed(&mut self, speed: f32) -> Result<(), NyaaError> {
        let previous = self.speed;

        for index in 0..self.members.len() {
            if let Err(error) = self.members[index].try_set_speed(speed) {
                for nyaa in &mut self.members[..index] {
                    let _ = nyaa.try_set_speed(previous);
                }
                return Err(error);
            }
        }

        self.speed = speed;
        Ok(())
    }

    /// Returns the playback speed applied to every member in the group.
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// Enables or disables pitch preservation for every current and future member.
    pub fn set_preserve_pitch(&mut self, preserve_pitch: bool) -> Result<(), NyaaError> {
        let previous = self.preserve_pitch;

        for index in 0..self.members.len() {
            if let Err(error) = self.members[index].set_preserve_pitch(preserve_pitch) {
                for nyaa in &mut self.members[..index] {
                    let _ = nyaa.set_preserve_pitch(previous);
                }
                return Err(error);
            }
        }

        self.preserve_pitch = preserve_pitch;
        Ok(())
    }

    /// Returns whether group speed changes preserve source pitch.
    pub fn preserves_pitch(&self) -> bool {
        self.preserve_pitch
    }

    /// Replaces the post-processing chain of every current and future member.
    pub fn set_effects(&mut self, effects: AudioEffects) -> Result<(), NyaaError> {
        effects.validate()?;
        let previous = self.effects;

        for index in 0..self.members.len() {
            if let Err(error) = self.members[index].set_effects(effects) {
                for nyaa in &mut self.members[..index] {
                    let _ = nyaa.set_effects(previous);
                }
                return Err(error);
            }
        }

        self.effects = effects;
        Ok(())
    }

    /// Returns the post-processing chain applied to every member in the group.
    pub fn effects(&self) -> AudioEffects {
        self.effects
    }

    /// Enables or disables looping for every current and future member.
    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
        for nyaa in &self.members {
            nyaa.set_looping(looping);
        }
    }

    /// Returns whether every member in the group repeats at its playback boundary.
    pub fn is_looping(&self) -> bool {
        self.looping
    }

    /// Sets the source-time playback range of every current and future member.
    pub fn set_loop_range(&mut self, range: Range<Duration>) -> Result<(), NyaaError> {
        if range.start >= range.end {
            return Err(NyaaError::InvalidPlaybackRange);
        }

        let previous = self.loop_range.clone();
        for index in 0..self.members.len() {
            if let Err(error) = self.members[index].set_loop_range(range.clone()) {
                for nyaa in &mut self.members[..index] {
                    if let Some(previous) = previous.clone() {
                        let _ = nyaa.set_loop_range(previous);
                    } else {
                        nyaa.clear_loop_range();
                    }
                }
                return Err(error);
            }
        }

        self.loop_range = Some(range);
        Ok(())
    }

    /// Returns the playback range applied to every member in the group.
    pub fn loop_range(&self) -> Option<Range<Duration>> {
        self.loop_range.clone()
    }

    /// Removes the playback range from every current and future member.
    pub fn clear_loop_range(&mut self) {
        self.loop_range = None;
        for nyaa in &mut self.members {
            nyaa.clear_loop_range();
        }
    }

    /// Ensures the shared audio output is initialized and reconnects every member to it.
    pub fn ensure_output(&mut self) -> Result<(), AudioOutputError> {
        self.output.retry_sink()?;
        for nyaa in &mut self.members {
            nyaa.retry_output()?;
        }
        Ok(())
    }

    /// Returns the backend selected for the group's shared output.
    pub fn backend(&self) -> Option<Backend> {
        self.output.backend()
    }

    /// Returns the device selected for the group's shared output.
    pub fn device(&self) -> Option<Device> {
        self.output.device()
    }

    /// Returns the backend the group prefers when opening its shared output.
    pub fn preferred_backend(&self) -> Option<Backend> {
        self.preferred_backend
    }

    /// Returns a UI-friendly name for the group's shared output.
    pub fn backend_display_name(&self) -> String {
        self.output.display_name()
    }

    fn replace_output(&mut self, output: Output, preferred_backend: Option<Backend>) {
        for nyaa in &mut self.members {
            nyaa.replace_output_preserving_playback(output.clone(), preferred_backend);
        }
        self.output = output;
        self.preferred_backend = preferred_backend;
    }

    /// Remembers a backend preference for the shared output. `None` follows the system default.
    pub fn set_preferred_backend(&mut self, backend: Option<Backend>) {
        if self.preferred_backend == backend && self.backend().is_some() {
            return;
        }

        self.replace_output(Output::new_with_preferred_backend(backend), backend);
    }

    /// Remembers a backend preference using [`Output::parse_backend_label`].
    ///
    /// Returns `false` without changing anything when the label is unknown.
    pub fn set_preferred_backend_by_name(&mut self, name: &str) -> bool {
        let Some(backend) = Output::parse_backend_label(name) else {
            return false;
        };

        self.set_preferred_backend(Some(backend));
        true
    }

    /// Switches every member to one newly opened shared backend.
    pub fn switch_backend(&mut self, backend: Backend) -> Result<(), AudioOutputError> {
        if self.backend() == Some(backend) {
            self.preferred_backend = Some(backend);
            for nyaa in &mut self.members {
                nyaa.switch_backend(backend)?;
            }
            return Ok(());
        }

        let output = Output::try_new_with_backend(backend)?;
        self.replace_output(output, Some(backend));
        Ok(())
    }

    /// Switches every member to one newly opened shared output device.
    pub fn switch_device(&mut self, device: &Device) -> Result<(), AudioOutputError> {
        if self.device().as_ref() == Some(device) {
            self.preferred_backend = Some(device.backend());
            for nyaa in &mut self.members {
                nyaa.switch_device(device)?;
            }
            return Ok(());
        }

        let output = Output::try_new_with_device(device)?;
        self.replace_output(output, Some(device.backend()));
        Ok(())
    }

    /// Switches every member to the process-wide preferred backend.
    ///
    /// Uses [`Output::global_preferred_backend`] when one is set, otherwise
    /// the system default backend. See [`NyaaGroup::switch_backend`] for
    /// failure semantics.
    pub fn switch_to_global_backend(&mut self) -> Result<(), AudioOutputError> {
        let backend =
            Output::global_preferred_backend().unwrap_or_else(|| rodio::cpal::default_host().id());
        self.switch_backend(backend)
    }

    /// Remembers the process-wide preferred backend for the shared output without failing.
    ///
    /// Equivalent to [`NyaaGroup::set_preferred_backend`] with
    /// [`Output::global_preferred_backend`]. `None` (no global preference)
    /// follows the system default.
    pub fn set_preferred_backend_to_global(&mut self) {
        self.set_preferred_backend(Output::global_preferred_backend());
    }

    /// Returns the first member's source-time position, or zero when the group is empty.
    pub fn position(&self) -> Duration {
        self.members.first().map_or(Duration::ZERO, Nyaa::position)
    }

    /// Returns the longest known source duration, or `None` if the group is empty or any duration
    /// is unknown.
    pub fn duration(&self) -> Option<Duration> {
        let mut longest = None;

        for nyaa in &self.members {
            let duration = nyaa.duration()?;
            longest = Some(longest.map_or(duration, |current: Duration| current.max(duration)));
        }

        longest
    }

    /// Returns `true` while at least one source in the group is playing.
    pub fn is_playing(&self) -> bool {
        self.members.iter().any(Nyaa::is_playing)
    }

    /// Returns `true` when the group has sources and every source is paused.
    pub fn is_paused(&self) -> bool {
        !self.members.is_empty() && self.members.iter().all(Nyaa::is_paused)
    }

    /// Returns the number of members currently owned by the group.
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Returns whether the group owns no members.
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Returns read-only access to the individual members for per-source state inspection.
    pub fn members(&self) -> &[Nyaa] {
        &self.members
    }

    /// Drops all members and loaded sources while retaining group configuration.
    pub fn clear(&mut self) {
        self.members.clear();
        self.member_keys.clear();
    }

    /// Blocks the current thread until every source has ended.
    pub fn wait_until_end(&self) {
        for nyaa in &self.members {
            nyaa.wait_until_end();
        }
    }
}

/// Switches every group to one newly opened shared backend.
///
/// This is a convenience for consumers that own several groups and do not
/// want to switch each one manually. Each group keeps its sources and
/// positions (see [`NyaaGroup::switch_backend`]). Stops at the first failure;
/// earlier groups remain switched. When the backend cannot be opened, the
/// first group fails without switching anything.
pub fn switch_groups_to_backend<'a>(
    groups: impl IntoIterator<Item = &'a mut NyaaGroup>,
    backend: Backend,
) -> Result<(), AudioOutputError> {
    for group in groups {
        group.switch_backend(backend)?;
    }

    Ok(())
}

/// Switches every group to one newly opened shared output device.
///
/// This is a convenience for consumers that own several groups and do not
/// want to switch each one manually. Each group keeps its sources and
/// positions (see [`NyaaGroup::switch_device`]). Stops at the first failure;
/// earlier groups remain switched.
pub fn switch_groups_to_device<'a>(
    groups: impl IntoIterator<Item = &'a mut NyaaGroup>,
    device: &Device,
) -> Result<(), AudioOutputError> {
    for group in groups {
        group.switch_device(device)?;
    }

    Ok(())
}

/// Switches every group to the process-wide preferred backend.
///
/// Uses [`Output::global_preferred_backend`] when one is set, otherwise the
/// system default backend. See [`switch_groups_to_backend`] for failure
/// semantics.
pub fn switch_groups_to_global_backend<'a>(
    groups: impl IntoIterator<Item = &'a mut NyaaGroup>,
) -> Result<(), AudioOutputError> {
    let backend =
        Output::global_preferred_backend().unwrap_or_else(|| rodio::cpal::default_host().id());
    switch_groups_to_backend(groups, backend)
}

/// Remembers a backend preference for every group without failing.
///
/// Equivalent to calling [`NyaaGroup::set_preferred_backend`] on each group.
/// `None` follows the process default (see [`Output::global_preferred_backend`]).
pub fn set_groups_preferred_backend<'a>(
    groups: impl IntoIterator<Item = &'a mut NyaaGroup>,
    backend: Option<Backend>,
) {
    for group in groups {
        group.set_preferred_backend(backend);
    }
}

/// Remembers the process-wide preferred backend for every group without failing.
///
/// Equivalent to calling [`NyaaGroup::set_preferred_backend_to_global`] on each group.
pub fn set_groups_preferred_backend_to_global<'a>(
    groups: impl IntoIterator<Item = &'a mut NyaaGroup>,
) {
    set_groups_preferred_backend(groups, Output::global_preferred_backend());
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::nyaa::{PlaybackEvent, PlaybackState};
    use std::thread;

    const TEST_AUDIO_BYTES: &[u8] = include_bytes!("../examples/THE UNFORGIVING.mp3");

    #[test]
    fn loaded_members_share_group_configuration() {
        let mut group = NyaaGroup::new_with_output(Output::new_deferred(None));
        let range = Duration::from_secs(1)..Duration::from_secs(2);
        let effects = AudioEffects {
            input_gain: 0.5,
            ..AudioEffects::default()
        };

        group.set_volume(0.35);
        group
            .try_set_speed(1.25)
            .expect("an idle group should accept a speed");
        group
            .set_preserve_pitch(true)
            .expect("an idle group should accept pitch preservation");
        group
            .set_effects(effects)
            .expect("the effect chain should be valid");
        group
            .set_loop_range(range.clone())
            .expect("the range should be valid");
        group.set_looping(true);
        group
            .load_static_bytes([TEST_AUDIO_BYTES, TEST_AUDIO_BYTES])
            .expect("both embedded sources should load");

        assert_eq!(group.len(), 2);
        for nyaa in group.members() {
            assert_eq!(nyaa.volume(), 0.35);
            assert_eq!(nyaa.speed(), 1.25);
            assert!(nyaa.preserves_pitch());
            assert_eq!(nyaa.effects(), effects);
            assert_eq!(nyaa.loop_range(), Some(range.clone()));
            assert!(nyaa.is_looping());
            assert_eq!(nyaa.state(), PlaybackState::Idle);
        }

        assert!(group.load_shared_bytes([[0_u8, 1, 2]]).is_err());
        assert_eq!(group.len(), 2, "a failed load should retain the old group");
        assert!(group.members().iter().all(|nyaa| nyaa.duration().is_some()));
    }

    #[test]
    fn group_transport_starts_and_controls_every_source() {
        let mut group = NyaaGroup::new();
        group
            .load_static_bytes([TEST_AUDIO_BYTES, TEST_AUDIO_BYTES])
            .expect("both embedded sources should load");

        group.play().expect("the loaded group should play");

        assert!(group.is_playing());
        for nyaa in group.members() {
            assert_eq!(nyaa.state(), PlaybackState::Playing);
            assert_eq!(
                nyaa.poll_event(),
                Some(PlaybackEvent::StateChanged {
                    previous: PlaybackState::Idle,
                    current: PlaybackState::Playing,
                })
            );
        }

        thread::sleep(Duration::from_millis(25));
        let positions: Vec<Duration> = group.members().iter().map(Nyaa::position).collect();
        let spread = positions
            .iter()
            .max()
            .unwrap()
            .saturating_sub(*positions.iter().min().unwrap());
        assert!(
            spread < Duration::from_millis(20),
            "group playback positions diverged by {spread:?}"
        );

        group.pause();
        assert!(group.is_paused());
        group
            .try_seek(Duration::from_secs(42))
            .expect("every source should seek");
        assert!(
            group
                .members()
                .iter()
                .all(|nyaa| nyaa.position() == Duration::from_secs(42))
        );

        group.resume();
        assert!(
            group
                .members()
                .iter()
                .all(|nyaa| nyaa.state() == PlaybackState::Playing)
        );

        group.stop();
        assert!(!group.is_playing());
        assert!(
            group
                .members()
                .iter()
                .all(|nyaa| nyaa.state() == PlaybackState::Idle)
        );
    }

    #[test]
    fn named_members_can_be_played_and_configured_independently() {
        let mut group = NyaaGroup::new();
        group
            .load_static_bytes_keyed([("music", TEST_AUDIO_BYTES), ("ambience", TEST_AUDIO_BYTES)])
            .expect("both named sources should load");

        assert_eq!(
            group.member_keys().collect::<Vec<_>>(),
            ["music", "ambience"]
        );
        group
            .member_mut("ambience")
            .expect("the ambience member should exist")
            .set_volume(0.2);

        group
            .play_member("music")
            .expect("the music member should play");
        assert!(group.member("music").is_some_and(Nyaa::is_playing));
        assert!(group.member("ambience").is_some_and(Nyaa::is_empty));

        group
            .play_member("ambience")
            .expect("the ambience member should play");
        assert_eq!(group.member("ambience").unwrap().volume(), 0.2);

        assert!(matches!(
            group.play_member("missing"),
            Err(NyaaError::UnknownGroupKey(key)) if key == "missing"
        ));
        assert!(matches!(
            group.add_static_bytes("music", TEST_AUDIO_BYTES),
            Err(NyaaError::DuplicateGroupKey(key)) if key == "music"
        ));
    }
}
