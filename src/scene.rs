use std::{
    collections::{HashSet, VecDeque},
    fmt,
    ops::Range,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use rodio::{
    Player, Source,
    mixer::{Mixer, MixerSource, mixer},
};

use crate::{
    Backend, Device, NyaaError, Output, OutputError, PlaybackEvent, PlaybackState, SoundEffects,
    SoundSource, sound::SoundNode,
};

const BUS_CHANNELS: u16 = 2;
const BUS_SAMPLE_RATE: u32 = 48_000;

/// Identifies a sound within one [`Nyaa`] instance.
///
/// IDs remain stable when other sounds are inserted or when the sound is moved. An ID becomes
/// invalid when its sound is removed, and a reused storage slot receives a new generation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SoundId {
    index: u32,
    generation: u32,
}

/// Identifies a sound group within one [`Nyaa`] instance.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SoundGroupId {
    index: u32,
    generation: u32,
}

/// Declares the complete set of sounds that a typed [`Nyaa`] scene must contain.
///
/// `ALL` must contain every possible value exactly once. Each path is relative to the scene root;
/// its final component names the sound and preceding components name its initial mixer groups.
pub trait SoundKey: Copy + Eq + 'static {
    /// Every key that must be registered before the scene can be built.
    const ALL: &'static [Self];

    /// Returns this sound's initial slash-separated path.
    fn path(self) -> &'static str;
}

/// Declares an enum implementing [`SoundKey`].
///
/// ```
/// rodisnyaa::sound_key! {
///     pub enum AppSound {
///         Preview => "preview",
///         BattleTheme => "music/battle/theme",
///     }
/// }
/// ```
#[macro_export]
macro_rules! sound_key {
    (
        $(#[$enum_meta:meta])*
        $visibility:vis enum $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident => $path:literal
            ),+ $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        $visibility enum $name {
            $($(#[$variant_meta])* $variant),+
        }

        impl $crate::SoundKey for $name {
            const ALL: &'static [Self] = &[$(Self::$variant),+];

            fn path(self) -> &'static str {
                match self {
                    $(Self::$variant => $path),+
                }
            }
        }
    };
}

/// A validated constructor for a typed [`Nyaa`] scene.
pub struct NyaaBuilder<K: SoundKey> {
    output: Output,
    preferred_backend: Option<Backend>,
    sounds: Vec<(K, SoundSource)>,
}

/// A playback event emitted by one sound in a [`Nyaa`] instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NyaaEvent {
    /// The sound that emitted the event.
    pub sound: SoundId,
    /// The playback lifecycle transition.
    pub event: PlaybackEvent,
}

/// An asynchronous sound failure discovered by [`Nyaa::update`].
#[derive(Debug)]
pub struct SoundError {
    /// The sound whose operation failed.
    pub sound: SoundId,
    /// The loading or playback error.
    pub error: NyaaError,
}

struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

struct Arena<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }
}

impl<T> Arena<T> {
    fn insert(&mut self, value: T) -> (u32, u32) {
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.value = Some(value);
            return (index, slot.generation);
        }

        let index = self.slots.len() as u32;
        self.slots.push(Slot {
            generation: 0,
            value: Some(value),
        });
        (index, 0)
    }

    fn get(&self, index: u32, generation: u32) -> Option<&T> {
        let slot = self.slots.get(index as usize)?;
        (slot.generation == generation)
            .then_some(slot.value.as_ref())
            .flatten()
    }

    fn get_mut(&mut self, index: u32, generation: u32) -> Option<&mut T> {
        let slot = self.slots.get_mut(index as usize)?;
        (slot.generation == generation)
            .then_some(slot.value.as_mut())
            .flatten()
    }

    fn remove(&mut self, index: u32, generation: u32) -> Option<T> {
        let slot = self.slots.get_mut(index as usize)?;
        if slot.generation != generation {
            return None;
        }

        let value = slot.value.take()?;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(index);
        Some(value)
    }

    fn ids(&self) -> Vec<(u32, u32)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                slot.value.as_ref().map(|_| (index as u32, slot.generation))
            })
            .collect()
    }
}

struct GroupEntry {
    name: String,
    parent: Option<SoundGroupId>,
    volume: f32,
    muted: bool,
    effects: SoundEffects,
    paused: bool,
    mixer: Mixer,
    bus_player: Option<Player>,
    pending_source: Option<Box<dyn Source<Item = f32> + Send>>,
}

struct NyaaState {
    output: Output,
    preferred_backend: Option<Backend>,
    groups: Arena<GroupEntry>,
    sounds: Arena<SoundNode>,
    required_sounds: HashSet<SoundId>,
    root: SoundGroupId,
    events: VecDeque<NyaaEvent>,
    event_capacity: usize,
}

fn new_bus() -> (Mixer, MixerSource) {
    mixer(
        rodio::ChannelCount::new(BUS_CHANNELS).expect("the bus channel count is non-zero"),
        rodio::SampleRate::new(BUS_SAMPLE_RATE).expect("the bus sample rate is non-zero"),
    )
}

fn validate_name(name: &str) -> Result<(), NyaaError> {
    if name.is_empty() || name.trim() != name || name.contains('/') {
        Err(NyaaError::InvalidName)
    } else {
        Ok(())
    }
}

fn validate_volume(volume: f32) -> Result<(), NyaaError> {
    if volume.is_finite() && volume >= 0.0 {
        Ok(())
    } else {
        Err(NyaaError::InvalidVolume)
    }
}

fn validate_speed(speed: f32) -> Result<(), NyaaError> {
    if speed.is_finite() && speed > 0.0 {
        Ok(())
    } else {
        Err(NyaaError::InvalidSpeed)
    }
}

impl NyaaState {
    fn new(output: Output, preferred_backend: Option<Backend>) -> Self {
        let (root_mixer, root_source) = new_bus();
        let mut groups = Arena::default();
        let (index, generation) = groups.insert(GroupEntry {
            name: String::new(),
            parent: None,
            volume: 1.0,
            muted: false,
            effects: SoundEffects::default(),
            paused: false,
            mixer: root_mixer,
            bus_player: None,
            pending_source: None,
        });
        let root = SoundGroupId { index, generation };
        let mut state = Self {
            output,
            preferred_backend,
            groups,
            sounds: Arena::default(),
            required_sounds: HashSet::new(),
            root,
            events: VecDeque::new(),
            event_capacity: usize::MAX,
        };
        state.attach_group_source(root, root_source);
        state
    }

    fn group(&self, id: SoundGroupId) -> Result<&GroupEntry, NyaaError> {
        self.groups
            .get(id.index, id.generation)
            .ok_or(NyaaError::InvalidSoundGroupHandle)
    }

    fn group_mut(&mut self, id: SoundGroupId) -> Result<&mut GroupEntry, NyaaError> {
        self.groups
            .get_mut(id.index, id.generation)
            .ok_or(NyaaError::InvalidSoundGroupHandle)
    }

    fn sound(&self, id: SoundId) -> Result<&SoundNode, NyaaError> {
        self.sounds
            .get(id.index, id.generation)
            .ok_or(NyaaError::InvalidSoundHandle)
    }

    fn sound_mut(&mut self, id: SoundId) -> Result<&mut SoundNode, NyaaError> {
        self.sounds
            .get_mut(id.index, id.generation)
            .ok_or(NyaaError::InvalidSoundHandle)
    }

    fn attach_group_source(&mut self, id: SoundGroupId, source: MixerSource) {
        let group = self.group(id).expect("newly created groups remain valid");
        let parent = group.parent;
        let effects = group.effects;
        let volume = if group.muted { 0.0 } else { group.volume };
        let source = effects.apply(source);
        let player = match parent {
            Some(parent) => {
                let parent_mixer = self
                    .group(parent)
                    .expect("a group's parent must remain valid")
                    .mixer
                    .clone();
                Some(Player::connect_new(&parent_mixer))
            }
            None => self.output.connect_player(),
        };

        let group = self
            .group_mut(id)
            .expect("newly created groups remain valid");
        if let Some(player) = player {
            player.set_volume(volume);
            player.append(source);
            player.play();
            group.bus_player = Some(player);
            group.pending_source = None;
        } else {
            group.bus_player = None;
            group.pending_source = Some(source);
        }
    }

    fn connect_pending_root(&mut self) {
        let source = self
            .group_mut(self.root)
            .expect("the root group always exists")
            .pending_source
            .take();
        let Some(source) = source else {
            return;
        };
        let Some(player) = self.output.connect_player() else {
            self.group_mut(self.root)
                .expect("the root group always exists")
                .pending_source = Some(source);
            return;
        };
        let root = self
            .group_mut(self.root)
            .expect("the root group always exists");
        player.set_volume(if root.muted { 0.0 } else { root.volume });
        player.append(source);
        player.play();
        root.bus_player = Some(player);
    }

    fn ensure_output(&mut self) -> Result<(), OutputError> {
        self.output.retry_sink()?;
        self.connect_pending_root();
        Ok(())
    }

    fn replace_output(&mut self, output: Output, preferred_backend: Option<Backend>) {
        self.output = output;
        self.preferred_backend = preferred_backend;
        self.rebuild_audio_graph();
    }

    fn rebuild_audio_graph(&mut self) {
        let group_ids = self
            .groups
            .ids()
            .into_iter()
            .map(|(index, generation)| SoundGroupId { index, generation })
            .collect::<Vec<_>>();
        let mut sources = Vec::with_capacity(group_ids.len());

        for id in &group_ids {
            let (new_mixer, source) = new_bus();
            let group = self
                .group_mut(*id)
                .expect("enumerated groups must remain valid");
            group.mixer = new_mixer;
            group.bus_player = None;
            group.pending_source = None;
            sources.push((*id, source));
        }

        for (id, source) in sources {
            self.attach_group_source(id, source);
        }

        let sound_ids = self
            .sounds
            .ids()
            .into_iter()
            .map(|(index, generation)| SoundId { index, generation })
            .collect::<Vec<_>>();

        for id in sound_ids {
            let group_id = self
                .sound(id)
                .expect("enumerated sounds must remain valid")
                .group;
            let mixer = self
                .group(group_id)
                .expect("a sound's group must remain valid")
                .mixer
                .clone();
            self.sound_mut(id)
                .expect("enumerated sounds must remain valid")
                .replace_routing_preserving_playback(mixer);
            self.collect_sound_events(id);
        }
    }

    fn has_child_name(
        &self,
        parent: SoundGroupId,
        name: &str,
        except_sound: Option<SoundId>,
        except_group: Option<SoundGroupId>,
    ) -> bool {
        self.groups.ids().into_iter().any(|(index, generation)| {
            let id = SoundGroupId { index, generation };
            Some(id) != except_group
                && self
                    .group(id)
                    .is_ok_and(|group| group.parent == Some(parent) && group.name == name)
        }) || self.sounds.ids().into_iter().any(|(index, generation)| {
            let id = SoundId { index, generation };
            Some(id) != except_sound
                && self
                    .sound(id)
                    .is_ok_and(|sound| sound.group == parent && sound.name == name)
        })
    }

    fn child_group(&self, parent: SoundGroupId, name: &str) -> Option<SoundGroupId> {
        self.groups
            .ids()
            .into_iter()
            .find_map(|(index, generation)| {
                let id = SoundGroupId { index, generation };
                self.group(id)
                    .is_ok_and(|group| group.parent == Some(parent) && group.name == name)
                    .then_some(id)
            })
    }

    fn child_sound(&self, parent: SoundGroupId, name: &str) -> Option<SoundId> {
        self.sounds
            .ids()
            .into_iter()
            .find_map(|(index, generation)| {
                let id = SoundId { index, generation };
                self.sound(id)
                    .is_ok_and(|sound| sound.group == parent && sound.name == name)
                    .then_some(id)
            })
    }

    fn resolve_group(&self, start: SoundGroupId, path: &str) -> Option<SoundGroupId> {
        if path.is_empty() || path == "/" {
            return Some(start);
        }
        let mut group = start;
        for component in path.split('/') {
            validate_name(component).ok()?;
            group = self.child_group(group, component)?;
        }
        Some(group)
    }

    fn resolve_sound(&self, start: SoundGroupId, path: &str) -> Option<SoundId> {
        let (groups, name) = path.rsplit_once('/').unwrap_or(("", path));
        validate_name(name).ok()?;
        let parent = self.resolve_group(start, groups)?;
        self.child_sound(parent, name)
    }

    fn group_path(&self, id: SoundGroupId) -> Result<String, NyaaError> {
        self.group(id)?;
        if id == self.root {
            return Ok(String::new());
        }
        let mut names = Vec::new();
        let mut current = id;
        while current != self.root {
            let group = self.group(current)?;
            names.push(group.name.as_str());
            current = group.parent.ok_or(NyaaError::InvalidSoundGroupHandle)?;
        }
        names.reverse();
        Ok(names.join("/"))
    }

    fn group_is_beneath(&self, mut group: SoundGroupId, ancestor: SoundGroupId) -> bool {
        loop {
            if group == ancestor {
                return true;
            }
            let Ok(entry) = self.group(group) else {
                return false;
            };
            let Some(parent) = entry.parent else {
                return false;
            };
            group = parent;
        }
    }

    fn group_is_paused(&self, mut group: SoundGroupId) -> bool {
        loop {
            let Ok(entry) = self.group(group) else {
                return false;
            };
            if entry.paused {
                return true;
            }
            let Some(parent) = entry.parent else {
                return false;
            };
            group = parent;
        }
    }

    fn group_effective_volume(&self, mut group: SoundGroupId) -> Result<f32, NyaaError> {
        let mut volume = 1.0;
        loop {
            let entry = self.group(group)?;
            if entry.muted {
                return Ok(0.0);
            }
            volume *= entry.volume;
            let Some(parent) = entry.parent else {
                return Ok(volume);
            };
            group = parent;
        }
    }

    fn sounds_beneath(&self, group: SoundGroupId) -> Vec<SoundId> {
        self.sounds
            .ids()
            .into_iter()
            .filter_map(|(index, generation)| {
                let id = SoundId { index, generation };
                self.sound(id)
                    .is_ok_and(|sound| self.group_is_beneath(sound.group, group))
                    .then_some(id)
            })
            .collect()
    }

    fn groups_beneath(&self, group: SoundGroupId) -> Vec<SoundGroupId> {
        self.groups
            .ids()
            .into_iter()
            .filter_map(|(index, generation)| {
                let id = SoundGroupId { index, generation };
                self.group_is_beneath(id, group).then_some(id)
            })
            .collect()
    }

    fn collect_sound_events(&mut self, id: SoundId) {
        loop {
            let event = match self.sound_mut(id) {
                Ok(sound) => sound.poll_scene_event(),
                Err(_) => return,
            };
            let Some(event) = event else {
                return;
            };

            let entry = self
                .sound_mut(id)
                .expect("a sound remains valid while collecting its events");
            if matches!(
                event,
                PlaybackEvent::StateChanged {
                    current: PlaybackState::Idle | PlaybackState::Ended | PlaybackState::Failed,
                    ..
                }
            ) {
                entry.wants_playing = false;
            }
            if self.event_capacity > 0 {
                if self.events.len() >= self.event_capacity {
                    self.events.pop_front();
                }
                self.events.push_back(NyaaEvent { sound: id, event });
            }
        }
    }

    fn reconcile_sound_pause(&mut self, id: SoundId) {
        let group_paused = match self.sound(id) {
            Ok(sound) => self.group_is_paused(sound.group),
            Err(_) => return,
        };
        let sound = self
            .sound_mut(id)
            .expect("a sound remains valid while reconciling pause state");
        if sound.wants_playing {
            if sound.locally_paused || group_paused {
                sound.pause();
            } else {
                sound.resume();
            }
        }
        self.collect_sound_events(id);
    }

    fn remove_sound(&mut self, id: SoundId) -> Result<(), NyaaError> {
        if self.required_sounds.contains(&id) {
            return Err(NyaaError::RequiredSound);
        }
        let mut sound = self
            .sounds
            .remove(id.index, id.generation)
            .ok_or(NyaaError::InvalidSoundHandle)?;
        sound.cancel_pending_playback();
        sound.stop();
        Ok(())
    }

    fn remove_group(&mut self, id: SoundGroupId) -> Result<(), NyaaError> {
        self.group(id)?;
        if id == self.root {
            return Err(NyaaError::RootSoundGroup);
        }

        let sounds = self.sounds_beneath(id);
        if sounds
            .iter()
            .any(|sound| self.required_sounds.contains(sound))
        {
            return Err(NyaaError::RequiredSound);
        }

        for sound in sounds {
            self.remove_sound(sound)?;
        }

        let mut groups = self.groups_beneath(id);
        groups
            .sort_by_key(|group| std::cmp::Reverse(self.group_path(*group).map_or(0, |p| p.len())));
        for group in groups {
            let mut entry = self
                .groups
                .remove(group.index, group.generation)
                .ok_or(NyaaError::InvalidSoundGroupHandle)?;
            if let Some(player) = entry.bus_player.as_mut() {
                player.stop();
            }
        }
        Ok(())
    }
}

/// The root owner of an application's audio scene.
///
/// A `Nyaa` owns one output, every [`Sound`], and a recursive hierarchy of [`SoundGroup`] mixer
/// buses. Sounds and groups are lightweight handles; applications normally need to store only
/// this root value.
pub struct Nyaa<K = ()> {
    inner: Arc<Mutex<NyaaState>>,
    required_sounds: Vec<(K, SoundId)>,
}

impl Default for Nyaa<()> {
    fn default() -> Self {
        Self::new()
    }
}

impl Nyaa<()> {
    /// Creates an empty audio scene using the default output.
    pub fn new() -> Self {
        Self::from_output(Output::new(), None)
    }

    /// Creates an empty audio scene and returns an error if the default output cannot be opened.
    pub fn try_new() -> Result<Self, OutputError> {
        let backend = rodio::cpal::default_host().id();
        Ok(Self::from_output(
            Output::try_new_with_backend(backend)?,
            None,
        ))
    }

    /// Creates an empty audio scene connected to an existing output.
    pub fn new_with_output(output: Output) -> Self {
        let preferred_backend = output.backend();
        Self::from_output(output, preferred_backend)
    }

    fn from_output(output: Output, preferred_backend: Option<Backend>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(NyaaState::new(output, preferred_backend))),
            required_sounds: Vec::new(),
        }
    }
}

impl<K> Nyaa<K> {
    fn sound_handle(&self, id: SoundId) -> Sound {
        Sound {
            nyaa: Arc::downgrade(&self.inner),
            id,
        }
    }

    fn group_handle(&self, id: SoundGroupId) -> SoundGroup {
        SoundGroup {
            nyaa: Arc::downgrade(&self.inner),
            id,
        }
    }

    fn create_group_in(
        inner: &Arc<Mutex<NyaaState>>,
        parent: SoundGroupId,
        name: impl Into<String>,
    ) -> Result<SoundGroup, NyaaError> {
        let name = name.into();
        validate_name(&name)?;
        let mut state = inner.lock().unwrap();
        state.group(parent)?;
        if state.has_child_name(parent, &name, None, None) {
            return Err(NyaaError::DuplicateName(name));
        }

        let (group_mixer, source) = new_bus();
        let (index, generation) = state.groups.insert(GroupEntry {
            name,
            parent: Some(parent),
            volume: 1.0,
            muted: false,
            effects: SoundEffects::default(),
            paused: false,
            mixer: group_mixer,
            bus_player: None,
            pending_source: None,
        });
        let id = SoundGroupId { index, generation };
        state.attach_group_source(id, source);
        Ok(SoundGroup {
            nyaa: Arc::downgrade(inner),
            id,
        })
    }

    fn create_sound_in(
        inner: &Arc<Mutex<NyaaState>>,
        group: SoundGroupId,
        name: impl Into<String>,
        source: impl Into<SoundSource>,
    ) -> Result<Sound, NyaaError> {
        let name = name.into();
        validate_name(&name)?;
        let source = source.into();
        let mut state = inner.lock().unwrap();
        state.group(group)?;
        if state.has_child_name(group, &name, None, None) {
            return Err(NyaaError::DuplicateName(name));
        }

        let group_mixer = state.group(group)?.mixer.clone();
        let sound = SoundNode::new(name, group, source, group_mixer)?;
        let (index, generation) = state.sounds.insert(sound);
        let id = SoundId { index, generation };
        Ok(Sound {
            nyaa: Arc::downgrade(inner),
            id,
        })
    }

    /// Returns the implicit root mixer group.
    pub fn root_group(&self) -> SoundGroup {
        let root = self.inner.lock().unwrap().root;
        self.group_handle(root)
    }

    /// Creates a mixer group directly beneath the root.
    pub fn create_group(&self, name: impl Into<String>) -> Result<SoundGroup, NyaaError> {
        let root = self.inner.lock().unwrap().root;
        Self::create_group_in(&self.inner, root, name)
    }

    /// Creates a sound directly beneath the root.
    pub fn create_sound(
        &self,
        name: impl Into<String>,
        source: impl Into<SoundSource>,
    ) -> Result<Sound, NyaaError> {
        let root = self.inner.lock().unwrap().root;
        Self::create_sound_in(&self.inner, root, name, source)
    }

    /// Finds a sound by its slash-separated path from the root.
    pub fn find_sound(&self, path: &str) -> Option<Sound> {
        let state = self.inner.lock().unwrap();
        let id = state.resolve_sound(state.root, path)?;
        Some(self.sound_handle(id))
    }

    /// Returns a sound handle for a live stable identity.
    pub fn sound_by_id(&self, id: SoundId) -> Option<Sound> {
        self.inner.lock().unwrap().sound(id).ok()?;
        Some(self.sound_handle(id))
    }

    /// Finds a group by its slash-separated path from the root.
    pub fn group(&self, path: &str) -> Option<SoundGroup> {
        let state = self.inner.lock().unwrap();
        let id = state.resolve_group(state.root, path)?;
        Some(self.group_handle(id))
    }

    /// Returns a group handle for a live stable identity.
    pub fn group_by_id(&self, id: SoundGroupId) -> Option<SoundGroup> {
        self.inner.lock().unwrap().group(id).ok()?;
        Some(self.group_handle(id))
    }

    /// Returns handles to sounds that are immediate children of the root.
    pub fn sounds(&self) -> Vec<Sound> {
        let state = self.inner.lock().unwrap();
        state
            .sounds
            .ids()
            .into_iter()
            .filter_map(|(index, generation)| {
                let id = SoundId { index, generation };
                state
                    .sound(id)
                    .is_ok_and(|sound| sound.group == state.root)
                    .then(|| self.sound_handle(id))
            })
            .collect()
    }

    /// Returns handles to groups that are immediate children of the root.
    pub fn groups(&self) -> Vec<SoundGroup> {
        let state = self.inner.lock().unwrap();
        state
            .groups
            .ids()
            .into_iter()
            .filter_map(|(index, generation)| {
                let id = SoundGroupId { index, generation };
                state
                    .group(id)
                    .is_ok_and(|group| group.parent == Some(state.root))
                    .then(|| self.group_handle(id))
            })
            .collect()
    }

    /// Removes a sound and invalidates all of its handles.
    pub fn remove_sound(&self, sound: &Sound) -> Result<(), NyaaError> {
        sound.ensure_same_nyaa(&self.inner)?;
        self.inner.lock().unwrap().remove_sound(sound.id)
    }

    /// Recursively removes a group, its child groups, and all descendant sounds.
    pub fn remove_group(&self, group: &SoundGroup) -> Result<(), NyaaError> {
        group.ensure_same_nyaa(&self.inner)?;
        self.inner.lock().unwrap().remove_group(group.id)
    }

    /// Advances asynchronous loads and discovers natural playback completion.
    ///
    /// The returned failures are asynchronous errors whose initiating [`Sound::play`] call had
    /// already returned. Lifecycle transitions are also available through [`Nyaa::poll_event`].
    pub fn update(&self) -> Vec<SoundError> {
        let mut state = self.inner.lock().unwrap();
        let ids = state
            .sounds
            .ids()
            .into_iter()
            .map(|(index, generation)| SoundId { index, generation })
            .collect::<Vec<_>>();
        let mut failures = Vec::new();

        for id in ids {
            let result = state
                .sound_mut(id)
                .ok()
                .and_then(SoundNode::poll_pending_playback);
            if let Some(result) = result {
                match result {
                    Ok(()) => {
                        if let Ok(sound) = state.sound_mut(id) {
                            sound.source_loaded = true;
                        }
                        state.reconcile_sound_pause(id);
                    }
                    Err(error) => failures.push(SoundError { sound: id, error }),
                }
            }

            if let Ok(sound) = state.sound_mut(id) {
                sound.state();
            }
            state.collect_sound_events(id);
        }
        failures
    }

    /// Returns the next global playback event.
    pub fn poll_event(&self) -> Option<NyaaEvent> {
        let mut state = self.inner.lock().unwrap();
        let ids = state
            .sounds
            .ids()
            .into_iter()
            .map(|(index, generation)| SoundId { index, generation })
            .collect::<Vec<_>>();
        for id in ids {
            state.collect_sound_events(id);
        }
        state.events.pop_front()
    }

    /// Sets the number of global events retained by this root.
    pub fn set_event_capacity(&self, capacity: usize) {
        let mut state = self.inner.lock().unwrap();
        state.event_capacity = capacity;
        while state.events.len() > capacity {
            state.events.pop_front();
        }
    }

    /// Sets the master volume multiplier.
    pub fn set_volume(&self, volume: f32) -> Result<(), NyaaError> {
        self.root_group().set_volume(volume)
    }

    /// Returns the master volume multiplier.
    pub fn volume(&self) -> f32 {
        self.root_group()
            .volume()
            .expect("the root sound group always exists")
    }

    /// Mutes or unmutes the complete scene.
    pub fn set_muted(&self, muted: bool) {
        self.root_group()
            .set_muted(muted)
            .expect("the root sound group always exists");
    }

    /// Returns whether the complete scene is muted at its root.
    pub fn is_muted(&self) -> bool {
        self.root_group()
            .is_muted()
            .expect("the root sound group always exists")
    }

    /// Replaces the root post-mix effect chain.
    pub fn set_effects(&self, effects: SoundEffects) -> Result<(), NyaaError> {
        self.root_group().set_effects(effects)
    }

    /// Returns the root post-mix effect chain.
    pub fn effects(&self) -> SoundEffects {
        self.root_group()
            .effects()
            .expect("the root sound group always exists")
    }

    /// Pauses every playing sound until the root is resumed.
    pub fn pause(&self) {
        self.root_group()
            .pause()
            .expect("the root sound group always exists");
    }

    /// Releases the root pause gate without resuming individually paused sounds.
    pub fn resume(&self) {
        self.root_group()
            .resume()
            .expect("the root sound group always exists");
    }

    /// Stops every sound and resets their positions.
    pub fn stop_all(&self) -> Result<(), NyaaError> {
        self.root_group().stop_all()
    }

    /// Returns the selected audio backend.
    pub fn backend(&self) -> Option<Backend> {
        self.inner.lock().unwrap().output.backend()
    }

    /// Returns the selected audio output device.
    pub fn device(&self) -> Option<Device> {
        self.inner.lock().unwrap().output.device()
    }

    /// Returns whether the physical audio output is initialized.
    pub fn has_output(&self) -> bool {
        self.inner.lock().unwrap().output.has_output()
    }

    /// Returns the backend preferred when opening the output.
    pub fn preferred_backend(&self) -> Option<Backend> {
        self.inner.lock().unwrap().preferred_backend
    }

    /// Returns a UI-friendly label for the active or preferred backend.
    pub fn backend_display_name(&self) -> String {
        let state = self.inner.lock().unwrap();
        if state.output.has_output() {
            state.output.display_name()
        } else {
            Output::backend_label(state.preferred_backend.unwrap_or_else(|| {
                state
                    .output
                    .backend()
                    .unwrap_or_else(|| rodio::cpal::default_host().id())
            }))
        }
    }

    /// Ensures the physical output is open and connects the root mixer to it.
    pub fn ensure_output(&self) -> Result<(), OutputError> {
        self.inner.lock().unwrap().ensure_output()
    }

    /// Remembers a backend preference and replaces the current output.
    pub fn set_preferred_backend(&self, backend: Option<Backend>) {
        let output = Output::new_with_preferred_backend(backend);
        self.inner.lock().unwrap().replace_output(output, backend);
    }

    /// Remembers a backend preference by its user-facing label.
    pub fn set_preferred_backend_by_name(&self, name: &str) -> bool {
        let Some(backend) = Output::parse_backend_label(name) else {
            return false;
        };
        self.set_preferred_backend(Some(backend));
        true
    }

    /// Switches the complete scene to a newly opened backend.
    pub fn switch_backend(&self, backend: Backend) -> Result<(), OutputError> {
        let output = Output::try_new_with_backend(backend)?;
        self.inner
            .lock()
            .unwrap()
            .replace_output(output, Some(backend));
        Ok(())
    }

    /// Switches the complete scene to a newly opened output device.
    pub fn switch_device(&self, device: &Device) -> Result<(), OutputError> {
        let output = Output::try_new_with_device(device)?;
        self.inner
            .lock()
            .unwrap()
            .replace_output(output, Some(device.backend()));
        Ok(())
    }

    /// Controls whether dropping the underlying device sink logs a message.
    pub fn log_on_drop(&self, log: bool) {
        self.inner.lock().unwrap().output.log_on_drop(log);
    }
}

impl<K: SoundKey> Nyaa<K> {
    /// Starts building a typed scene using the default output.
    pub fn builder() -> NyaaBuilder<K> {
        NyaaBuilder::new()
    }

    /// Starts building a typed scene connected to an existing output.
    pub fn builder_with_output(output: Output) -> NyaaBuilder<K> {
        NyaaBuilder::with_output(output)
    }

    /// Returns the sound identified by a required key.
    ///
    /// Unlike [`Self::find_sound`], this is total because [`NyaaBuilder::build`] validates that
    /// every key exists and required sounds cannot be removed.
    pub fn sound(&self, key: K) -> Sound {
        let id = self
            .required_sounds
            .iter()
            .find_map(|(candidate, id)| (*candidate == key).then_some(*id))
            .expect("SoundKey::ALL must contain every possible key");
        self.sound_handle(id)
    }
}

impl<K: SoundKey> Default for NyaaBuilder<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: SoundKey> NyaaBuilder<K> {
    /// Creates a typed scene builder using the default output.
    pub fn new() -> Self {
        Self {
            output: Output::new(),
            preferred_backend: None,
            sounds: Vec::new(),
        }
    }

    /// Creates a typed scene builder connected to an existing output.
    pub fn with_output(output: Output) -> Self {
        let preferred_backend = output.backend();
        Self {
            output,
            preferred_backend,
            sounds: Vec::new(),
        }
    }

    /// Registers the source for one required sound.
    ///
    /// Registering the same key again replaces its earlier source.
    #[must_use]
    pub fn sound(mut self, key: K, source: impl Into<SoundSource>) -> Self {
        let source = source.into();
        if let Some((_, registered)) = self
            .sounds
            .iter_mut()
            .find(|(candidate, _)| *candidate == key)
        {
            *registered = source;
        } else {
            self.sounds.push((key, source));
        }
        self
    }

    /// Registers an empty sound for a key whose audio resource will be assigned later.
    #[must_use]
    pub fn placeholder(self, key: K) -> Self {
        self.sound(key, SoundSource::Empty)
    }

    /// Constructs the scene while filling every unregistered key with an empty sound.
    ///
    /// Explicit sources and placeholders already registered on this builder are preserved.
    pub fn placeholders(mut self) -> Result<Nyaa<K>, NyaaError> {
        for &key in K::ALL {
            if !self.sounds.iter().any(|(candidate, _)| *candidate == key) {
                self.sounds.push((key, SoundSource::Empty));
            }
        }
        self.build()
    }

    /// Validates the schema and constructs a scene containing every required sound.
    pub fn build(mut self) -> Result<Nyaa<K>, NyaaError> {
        let dynamic = Nyaa::from_output(self.output, self.preferred_backend);
        let mut required_sounds = Vec::with_capacity(K::ALL.len());
        let mut paths = HashSet::with_capacity(K::ALL.len());

        for &key in K::ALL {
            let path = key.path();
            if path.starts_with('/') {
                return Err(NyaaError::InvalidName);
            }
            if !paths.insert(path) {
                return Err(NyaaError::DuplicateRequiredSoundPath(path));
            }
            let Some(index) = self
                .sounds
                .iter()
                .position(|(candidate, _)| *candidate == key)
            else {
                return Err(NyaaError::MissingRequiredSound(path));
            };
            let (_, source) = self.sounds.swap_remove(index);
            let (groups, name) = path.rsplit_once('/').unwrap_or(("", path));
            validate_name(name)?;

            let mut parent = dynamic.root_group();
            if !groups.is_empty() {
                for component in groups.split('/') {
                    validate_name(component)?;
                    parent = match parent.group(component) {
                        Some(group) => group,
                        None => parent.create_group(component)?,
                    };
                }
            }

            let sound = parent.create_sound(name, source)?;
            required_sounds.push((key, sound.id()));
        }

        if let Some((key, _)) = self.sounds.first() {
            return Err(NyaaError::UnknownRequiredSound(key.path()));
        }

        {
            let mut state = dynamic.inner.lock().unwrap();
            state
                .required_sounds
                .extend(required_sounds.iter().map(|(_, id)| *id));
        }

        Ok(Nyaa {
            inner: dynamic.inner,
            required_sounds,
        })
    }
}

/// A playable sound owned by a [`Nyaa`] audio scene.
#[derive(Clone)]
pub struct Sound {
    nyaa: Weak<Mutex<NyaaState>>,
    id: SoundId,
}

impl fmt::Debug for Sound {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Sound")
            .field("id", &self.id)
            .finish()
    }
}

impl Sound {
    fn state(&self) -> Result<Arc<Mutex<NyaaState>>, NyaaError> {
        self.nyaa.upgrade().ok_or(NyaaError::InvalidSoundHandle)
    }

    fn ensure_same_nyaa(&self, nyaa: &Arc<Mutex<NyaaState>>) -> Result<(), NyaaError> {
        if Weak::ptr_eq(&self.nyaa, &Arc::downgrade(nyaa)) {
            Ok(())
        } else {
            Err(NyaaError::DifferentNyaa)
        }
    }

    /// Returns this sound's stable identity.
    pub fn id(&self) -> SoundId {
        self.id
    }

    /// Returns this sound's name.
    pub fn name(&self) -> Result<String, NyaaError> {
        Ok(self.state()?.lock().unwrap().sound(self.id)?.name.clone())
    }

    /// Returns this sound's slash-separated path from the root.
    pub fn path(&self) -> Result<String, NyaaError> {
        let state = self.state()?;
        let state = state.lock().unwrap();
        let sound = state.sound(self.id)?;
        let group = state.group_path(sound.group)?;
        Ok(if group.is_empty() {
            sound.name.clone()
        } else {
            format!("{group}/{}", sound.name)
        })
    }

    /// Returns the mixer group to which this sound is assigned.
    pub fn group(&self) -> Result<SoundGroup, NyaaError> {
        let state = self.state()?;
        let group = state.lock().unwrap().sound(self.id)?.group;
        Ok(SoundGroup {
            nyaa: Arc::downgrade(&state),
            id: group,
        })
    }

    /// Moves this sound to another mixer group while preserving playback.
    pub fn set_group(&self, group: &SoundGroup) -> Result<(), NyaaError> {
        let state = self.state()?;
        group.ensure_same_nyaa(&state)?;
        let mut state = state.lock().unwrap();
        state.group(group.id)?;
        let name = state.sound(self.id)?.name.clone();
        if state.has_child_name(group.id, &name, Some(self.id), None) {
            return Err(NyaaError::DuplicateName(name));
        }
        if state.sound(self.id)?.group == group.id {
            return Ok(());
        }

        let mixer = state.group(group.id)?.mixer.clone();
        state.sound_mut(self.id)?.group = group.id;
        state
            .sound_mut(self.id)?
            .replace_routing_preserving_playback(mixer);
        state.reconcile_sound_pause(self.id);
        Ok(())
    }

    /// Returns a clone of the encoded source descriptor.
    pub fn source(&self) -> Result<SoundSource, NyaaError> {
        Ok(self.state()?.lock().unwrap().sound(self.id)?.source.clone())
    }

    /// Replaces the encoded source without starting playback.
    ///
    /// Setting the same underlying resource again is a no-op and preserves the current timeline.
    pub fn set_source(&self, source: impl Into<SoundSource>) -> Result<(), NyaaError> {
        let source = source.into();
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        let sound = state.sound_mut(self.id)?;
        sound.replace_source(source)?;
        state.collect_sound_events(self.id);
        Ok(())
    }

    /// Loads and decodes the source without starting playback.
    pub async fn load(&self) -> Result<(), NyaaError> {
        let state = self.state()?;

        #[cfg(target_arch = "wasm32")]
        {
            let (source, revision) = {
                let state = state.lock().unwrap();
                let sound = state.sound(self.id)?;
                (sound.source.clone(), sound.source_revision)
            };
            if let SoundSource::Asset(asset) = source {
                let bytes = asset.load_browser_bytes().await?;
                let mut state = state.lock().unwrap();
                let sound = state.sound_mut(self.id)?;
                if sound.source_revision != revision {
                    return Err(NyaaError::SoundSourceChanged);
                }
                sound.cancel_pending_playback();
                sound.load_resolved_bytes(bytes)?;
                sound.source_loaded = true;
                sound.wants_playing = false;
                sound.locally_paused = false;
                state.collect_sound_events(self.id);
                return Ok(());
            }
        }

        let mut state = state.lock().unwrap();
        let sound = state.sound_mut(self.id)?;
        sound.source_loaded = sound.load_source()?;
        sound.wants_playing = false;
        sound.locally_paused = false;
        state.collect_sound_events(self.id);
        Ok(())
    }

    /// Ensures this sound is playing, resuming paused playback when possible.
    pub fn play(&self) -> Result<(), NyaaError> {
        self.play_from_current(false)
    }

    /// Starts this sound again from the beginning.
    pub fn replay(&self) -> Result<(), NyaaError> {
        self.play_from_current(true)
    }

    fn play_from_current(&self, restart: bool) -> Result<(), NyaaError> {
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        state.ensure_output()?;
        let group_paused = state.group_is_paused(state.sound(self.id)?.group);
        let sound = state.sound_mut(self.id)?;

        if sound.wants_playing && sound.is_playing() && !restart {
            return Ok(());
        }

        if !sound.source_loaded {
            match &sound.source {
                SoundSource::Empty => return Err(NyaaError::NoAudioSource),
                SoundSource::Asset(asset) => {
                    let asset = asset.clone();
                    sound.start_asset_playback(&asset)?;
                    if !sound.is_loading() {
                        sound.source_loaded = true;
                    }
                }
                _ => {
                    sound.source_loaded = sound.load_source()?;
                }
            }
        }

        if sound.source_loaded {
            let state = sound.state();
            let position = if restart || state == PlaybackState::Ended {
                Duration::ZERO
            } else {
                sound.position()
            };
            sound.try_play_at(position)?;
        }

        sound.wants_playing = true;
        sound.locally_paused = false;
        if group_paused && !sound.is_loading() {
            sound.pause();
        }
        state.collect_sound_events(self.id);
        Ok(())
    }

    /// Pauses this sound independently of its group pause gates.
    pub fn pause(&self) -> Result<(), NyaaError> {
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        let sound = state.sound_mut(self.id)?;
        sound.locally_paused = true;
        sound.pause();
        state.collect_sound_events(self.id);
        Ok(())
    }

    /// Resumes this sound unless one of its ancestor groups remains paused.
    pub fn resume(&self) -> Result<(), NyaaError> {
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        let group_paused = state.group_is_paused(state.sound(self.id)?.group);
        let sound = state.sound_mut(self.id)?;
        sound.locally_paused = false;
        if sound.wants_playing && !group_paused {
            sound.resume();
        }
        state.collect_sound_events(self.id);
        Ok(())
    }

    /// Stops this sound, retains its source, and resets its position.
    pub fn stop(&self) -> Result<(), NyaaError> {
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        let sound = state.sound_mut(self.id)?;
        sound.cancel_pending_playback();
        sound.stop();
        sound.try_seek(Duration::ZERO)?;
        sound.wants_playing = false;
        sound.locally_paused = false;
        state.collect_sound_events(self.id);
        Ok(())
    }

    /// Seeks within this sound without changing its intended pause state.
    pub fn try_seek(&self, position: Duration) -> Result<(), NyaaError> {
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        state.sound_mut(self.id)?.try_seek(position)?;
        state.collect_sound_events(self.id);
        Ok(())
    }

    /// Seeks within this sound using seconds.
    pub fn try_seek_secs(&self, position_secs: f64) -> Result<(), NyaaError> {
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        state.sound_mut(self.id)?.try_seek_secs(position_secs)?;
        state.collect_sound_events(self.id);
        Ok(())
    }

    /// Returns the current playback lifecycle state.
    pub fn playback_state(&self) -> Result<PlaybackState, NyaaError> {
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        let playback_state = state.sound_mut(self.id)?.state();
        state.collect_sound_events(self.id);
        Ok(playback_state)
    }

    /// Returns the next event for this sound.
    pub fn poll_event(&self) -> Result<Option<PlaybackEvent>, NyaaError> {
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        state.sound_mut(self.id)?.state();
        state.collect_sound_events(self.id);
        Ok(state.sound_mut(self.id)?.poll_event())
    }

    /// Sets the number of events retained by this sound handle's event stream.
    pub fn set_event_capacity(&self, capacity: usize) -> Result<(), NyaaError> {
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        let sound = state.sound_mut(self.id)?;
        sound.set_event_capacity(capacity);
        Ok(())
    }

    /// Returns whether this sound is currently loading.
    pub fn is_loading(&self) -> Result<bool, NyaaError> {
        Ok(self.playback_state()? == PlaybackState::Loading)
    }

    /// Returns whether this sound is currently playing.
    pub fn is_playing(&self) -> Result<bool, NyaaError> {
        Ok(self.playback_state()? == PlaybackState::Playing)
    }

    /// Returns whether this sound is currently paused.
    pub fn is_paused(&self) -> Result<bool, NyaaError> {
        Ok(self.playback_state()? == PlaybackState::Paused)
    }

    /// Returns the current playback position.
    pub fn position(&self) -> Result<Duration, NyaaError> {
        Ok(self.state()?.lock().unwrap().sound(self.id)?.position())
    }

    /// Returns the current playback position formatted as `H:MM:SS` or `M:SS`.
    pub fn position_formatted(&self) -> Result<String, NyaaError> {
        Ok(self
            .state()?
            .lock()
            .unwrap()
            .sound(self.id)?
            .position_formatted())
    }

    /// Returns the source duration when known.
    pub fn duration(&self) -> Result<Option<Duration>, NyaaError> {
        Ok(self.state()?.lock().unwrap().sound(self.id)?.duration())
    }

    /// Returns the source duration formatted as `H:MM:SS` or `M:SS`.
    pub fn duration_formatted(&self) -> Result<String, NyaaError> {
        Ok(self
            .state()?
            .lock()
            .unwrap()
            .sound(self.id)?
            .duration_formatted())
    }

    /// Returns the current position clamped to the known duration.
    pub fn clamped_position(&self) -> Result<Duration, NyaaError> {
        Ok(self
            .state()?
            .lock()
            .unwrap()
            .sound(self.id)?
            .clamped_position())
    }

    /// Returns a range suitable for a seek control.
    pub fn seek_range(&self) -> Result<std::ops::RangeInclusive<f64>, NyaaError> {
        Ok(self.state()?.lock().unwrap().sound(self.id)?.seek_range())
    }

    /// Sets this sound's volume before group and master multipliers.
    pub fn set_volume(&self, volume: f32) -> Result<(), NyaaError> {
        validate_volume(volume)?;
        self.state()?
            .lock()
            .unwrap()
            .sound_mut(self.id)?
            .set_volume(volume);
        Ok(())
    }

    /// Returns this sound's local volume multiplier.
    pub fn volume(&self) -> Result<f32, NyaaError> {
        Ok(self.state()?.lock().unwrap().sound(self.id)?.volume())
    }

    /// Returns this sound's volume after all group and master multipliers.
    pub fn effective_volume(&self) -> Result<f32, NyaaError> {
        let state = self.state()?;
        let state = state.lock().unwrap();
        let sound = state.sound(self.id)?;
        Ok(sound.volume() * state.group_effective_volume(sound.group)?)
    }

    /// Sets this sound's playback speed.
    pub fn set_speed(&self, speed: f32) -> Result<(), NyaaError> {
        validate_speed(speed)?;
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        state.sound_mut(self.id)?.try_set_speed(speed)?;
        state.collect_sound_events(self.id);
        Ok(())
    }

    /// Returns this sound's playback speed.
    pub fn speed(&self) -> Result<f32, NyaaError> {
        Ok(self.state()?.lock().unwrap().sound(self.id)?.speed())
    }

    /// Enables or disables pitch preservation for this sound.
    pub fn set_preserve_pitch(&self, preserve_pitch: bool) -> Result<(), NyaaError> {
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        state
            .sound_mut(self.id)?
            .set_preserve_pitch(preserve_pitch)?;
        state.collect_sound_events(self.id);
        Ok(())
    }

    /// Returns whether this sound preserves pitch while changing speed.
    pub fn preserves_pitch(&self) -> Result<bool, NyaaError> {
        Ok(self
            .state()?
            .lock()
            .unwrap()
            .sound(self.id)?
            .preserves_pitch())
    }

    /// Sets this sound's pre-mix effect chain.
    pub fn set_effects(&self, effects: SoundEffects) -> Result<(), NyaaError> {
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        state.sound_mut(self.id)?.set_effects(effects)?;
        state.collect_sound_events(self.id);
        Ok(())
    }

    /// Returns this sound's pre-mix effect chain.
    pub fn effects(&self) -> Result<SoundEffects, NyaaError> {
        Ok(self.state()?.lock().unwrap().sound(self.id)?.effects())
    }

    /// Enables or disables looping for this sound.
    pub fn set_looping(&self, looping: bool) -> Result<(), NyaaError> {
        self.state()?
            .lock()
            .unwrap()
            .sound(self.id)?
            .set_looping(looping);
        Ok(())
    }

    /// Returns whether this sound loops.
    pub fn is_looping(&self) -> Result<bool, NyaaError> {
        Ok(self.state()?.lock().unwrap().sound(self.id)?.is_looping())
    }

    /// Sets this sound's playback and looping range.
    pub fn set_loop_range(&self, range: Range<Duration>) -> Result<(), NyaaError> {
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        state.sound_mut(self.id)?.set_loop_range(range)?;
        state.collect_sound_events(self.id);
        Ok(())
    }

    /// Returns this sound's playback and looping range.
    pub fn loop_range(&self) -> Result<Option<Range<Duration>>, NyaaError> {
        Ok(self.state()?.lock().unwrap().sound(self.id)?.loop_range())
    }

    /// Removes this sound's playback and looping range.
    pub fn clear_loop_range(&self) -> Result<(), NyaaError> {
        self.state()?
            .lock()
            .unwrap()
            .sound_mut(self.id)?
            .clear_loop_range();
        Ok(())
    }

    /// Blocks the current thread until this sound reaches the end.
    pub fn wait_until_end(&self) -> Result<(), NyaaError> {
        self.state()?
            .lock()
            .unwrap()
            .sound(self.id)?
            .wait_until_end();
        Ok(())
    }

    /// Removes this sound from its owning [`Nyaa`].
    pub fn remove(self) -> Result<(), NyaaError> {
        self.state()?.lock().unwrap().remove_sound(self.id)
    }
}

/// A recursive mixer group owned by a [`Nyaa`] audio scene.
#[derive(Clone)]
pub struct SoundGroup {
    nyaa: Weak<Mutex<NyaaState>>,
    id: SoundGroupId,
}

impl fmt::Debug for SoundGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SoundGroup")
            .field("id", &self.id)
            .finish()
    }
}

impl SoundGroup {
    fn state(&self) -> Result<Arc<Mutex<NyaaState>>, NyaaError> {
        self.nyaa
            .upgrade()
            .ok_or(NyaaError::InvalidSoundGroupHandle)
    }

    fn ensure_same_nyaa(&self, nyaa: &Arc<Mutex<NyaaState>>) -> Result<(), NyaaError> {
        if Weak::ptr_eq(&self.nyaa, &Arc::downgrade(nyaa)) {
            Ok(())
        } else {
            Err(NyaaError::DifferentNyaa)
        }
    }

    /// Returns this group's stable identity.
    pub fn id(&self) -> SoundGroupId {
        self.id
    }

    /// Returns this group's name, or an empty string for the implicit root.
    pub fn name(&self) -> Result<String, NyaaError> {
        Ok(self.state()?.lock().unwrap().group(self.id)?.name.clone())
    }

    /// Returns this group's slash-separated path from the root.
    pub fn path(&self) -> Result<String, NyaaError> {
        self.state()?.lock().unwrap().group_path(self.id)
    }

    /// Returns this group's parent, or `None` for the root.
    pub fn parent(&self) -> Result<Option<SoundGroup>, NyaaError> {
        let state = self.state()?;
        let parent = state.lock().unwrap().group(self.id)?.parent;
        Ok(parent.map(|id| SoundGroup {
            nyaa: Arc::downgrade(&state),
            id,
        }))
    }

    /// Moves this group beneath another group and rebuilds its mixer route.
    pub fn set_parent(&self, parent: &SoundGroup) -> Result<(), NyaaError> {
        let state = self.state()?;
        parent.ensure_same_nyaa(&state)?;
        let mut state = state.lock().unwrap();
        state.group(self.id)?;
        state.group(parent.id)?;
        if self.id == state.root {
            return Err(NyaaError::RootSoundGroup);
        }
        if state.group_is_beneath(parent.id, self.id) {
            return Err(NyaaError::SoundGroupCycle);
        }
        let name = state.group(self.id)?.name.clone();
        if state.has_child_name(parent.id, &name, None, Some(self.id)) {
            return Err(NyaaError::DuplicateName(name));
        }
        if state.group(self.id)?.parent == Some(parent.id) {
            return Ok(());
        }
        state.group_mut(self.id)?.parent = Some(parent.id);
        state.rebuild_audio_graph();
        let sounds = state.sounds_beneath(self.id);
        for sound in sounds {
            state.reconcile_sound_pause(sound);
        }
        Ok(())
    }

    /// Creates a child mixer group.
    pub fn create_group(&self, name: impl Into<String>) -> Result<SoundGroup, NyaaError> {
        let state = self.state()?;
        Nyaa::<()>::create_group_in(&state, self.id, name)
    }

    /// Creates a sound assigned to this mixer group.
    pub fn create_sound(
        &self,
        name: impl Into<String>,
        source: impl Into<SoundSource>,
    ) -> Result<Sound, NyaaError> {
        let state = self.state()?;
        Nyaa::<()>::create_sound_in(&state, self.id, name, source)
    }

    /// Finds a descendant sound by a path relative to this group.
    pub fn sound(&self, path: &str) -> Option<Sound> {
        let state = self.nyaa.upgrade()?;
        let id = state.lock().ok()?.resolve_sound(self.id, path)?;
        Some(Sound {
            nyaa: Arc::downgrade(&state),
            id,
        })
    }

    /// Finds a descendant group by a path relative to this group.
    pub fn group(&self, path: &str) -> Option<SoundGroup> {
        let state = self.nyaa.upgrade()?;
        let id = state.lock().ok()?.resolve_group(self.id, path)?;
        Some(SoundGroup {
            nyaa: Arc::downgrade(&state),
            id,
        })
    }

    /// Returns handles to this group's immediate sounds.
    pub fn sounds(&self) -> Result<Vec<Sound>, NyaaError> {
        let state = self.state()?;
        let guard = state.lock().unwrap();
        guard.group(self.id)?;
        Ok(guard
            .sounds
            .ids()
            .into_iter()
            .filter_map(|(index, generation)| {
                let id = SoundId { index, generation };
                guard
                    .sound(id)
                    .is_ok_and(|sound| sound.group == self.id)
                    .then_some(Sound {
                        nyaa: Arc::downgrade(&state),
                        id,
                    })
            })
            .collect())
    }

    /// Returns handles to this group's immediate child groups.
    pub fn groups(&self) -> Result<Vec<SoundGroup>, NyaaError> {
        let state = self.state()?;
        let guard = state.lock().unwrap();
        guard.group(self.id)?;
        Ok(guard
            .groups
            .ids()
            .into_iter()
            .filter_map(|(index, generation)| {
                let id = SoundGroupId { index, generation };
                guard
                    .group(id)
                    .is_ok_and(|group| group.parent == Some(self.id))
                    .then_some(SoundGroup {
                        nyaa: Arc::downgrade(&state),
                        id,
                    })
            })
            .collect())
    }

    /// Sets this group's volume multiplier.
    pub fn set_volume(&self, volume: f32) -> Result<(), NyaaError> {
        validate_volume(volume)?;
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        let group = state.group_mut(self.id)?;
        group.volume = volume;
        if let Some(player) = group.bus_player.as_ref() {
            player.set_volume(if group.muted { 0.0 } else { volume });
        }
        Ok(())
    }

    /// Returns this group's local volume multiplier.
    pub fn volume(&self) -> Result<f32, NyaaError> {
        Ok(self.state()?.lock().unwrap().group(self.id)?.volume)
    }

    /// Returns this group's volume after all ancestor and master multipliers.
    pub fn effective_volume(&self) -> Result<f32, NyaaError> {
        let state = self.state()?;
        let state = state.lock().unwrap();
        state.group_effective_volume(self.id)
    }

    /// Mutes or unmutes this group without changing its volume.
    pub fn set_muted(&self, muted: bool) -> Result<(), NyaaError> {
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        let group = state.group_mut(self.id)?;
        group.muted = muted;
        if let Some(player) = group.bus_player.as_ref() {
            player.set_volume(if muted { 0.0 } else { group.volume });
        }
        Ok(())
    }

    /// Returns whether this group is locally muted.
    pub fn is_muted(&self) -> Result<bool, NyaaError> {
        Ok(self.state()?.lock().unwrap().group(self.id)?.muted)
    }

    /// Replaces this group's post-mix effect chain.
    pub fn set_effects(&self, effects: SoundEffects) -> Result<(), NyaaError> {
        effects.validate()?;
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        if state.group(self.id)?.effects == effects {
            return Ok(());
        }
        state.group_mut(self.id)?.effects = effects;
        state.rebuild_audio_graph();
        Ok(())
    }

    /// Returns this group's post-mix effect chain.
    pub fn effects(&self) -> Result<SoundEffects, NyaaError> {
        Ok(self.state()?.lock().unwrap().group(self.id)?.effects)
    }

    /// Pauses all descendant sounds through a persistent group pause gate.
    pub fn pause(&self) -> Result<(), NyaaError> {
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        state.group_mut(self.id)?.paused = true;
        let sounds = state.sounds_beneath(self.id);
        for sound in sounds {
            state.reconcile_sound_pause(sound);
        }
        Ok(())
    }

    /// Releases this group's pause gate without releasing other group or sound-local pauses.
    pub fn resume(&self) -> Result<(), NyaaError> {
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        state.group_mut(self.id)?.paused = false;
        let sounds = state.sounds_beneath(self.id);
        for sound in sounds {
            state.reconcile_sound_pause(sound);
        }
        Ok(())
    }

    /// Returns whether this group is locally paused.
    pub fn is_paused(&self) -> Result<bool, NyaaError> {
        Ok(self.state()?.lock().unwrap().group(self.id)?.paused)
    }

    /// Returns whether this group or one of its ancestors is paused.
    pub fn is_effectively_paused(&self) -> Result<bool, NyaaError> {
        let state = self.state()?;
        let state = state.lock().unwrap();
        state.group(self.id)?;
        Ok(state.group_is_paused(self.id))
    }

    /// Stops all descendant sounds and resets their positions.
    pub fn stop_all(&self) -> Result<(), NyaaError> {
        let state = self.state()?;
        let mut state = state.lock().unwrap();
        state.group(self.id)?;
        let sounds = state.sounds_beneath(self.id);
        for id in sounds {
            let sound = state.sound_mut(id)?;
            sound.cancel_pending_playback();
            sound.stop();
            sound.try_seek(Duration::ZERO)?;
            sound.wants_playing = false;
            sound.locally_paused = false;
            state.collect_sound_events(id);
        }
        Ok(())
    }

    /// Restarts all descendant sounds from the beginning.
    pub fn replay_all(&self) -> Result<(), NyaaError> {
        let sounds = {
            let state = self.state()?;
            let state = state.lock().unwrap();
            state.group(self.id)?;
            state.sounds_beneath(self.id)
        };
        let state = self.state()?;
        for id in sounds {
            Sound {
                nyaa: Arc::downgrade(&state),
                id,
            }
            .replay()?;
        }
        Ok(())
    }

    /// Recursively removes this group and invalidates all descendant handles.
    pub fn remove(self) -> Result<(), NyaaError> {
        self.state()?.lock().unwrap().remove_group(self.id)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    const TEST_AUDIO_BYTES: &[u8] = include_bytes!("../examples/THE UNFORGIVING.mp3");

    sound_key! {
        enum TestSound {
            Preview => "preview",
            BattleTheme => "music/battle/theme",
        }
    }

    #[test]
    fn typed_scene_requires_every_key_and_returns_required_sounds_directly() {
        let missing = Nyaa::<TestSound>::builder_with_output(Output::new_deferred(None))
            .sound(
                TestSound::Preview,
                SoundSource::static_bytes(TEST_AUDIO_BYTES),
            )
            .build();
        assert!(matches!(
            missing,
            Err(NyaaError::MissingRequiredSound("music/battle/theme"))
        ));

        let nyaa = Nyaa::<TestSound>::builder_with_output(Output::new_deferred(None))
            .sound(
                TestSound::Preview,
                SoundSource::static_bytes(TEST_AUDIO_BYTES),
            )
            .sound(
                TestSound::BattleTheme,
                SoundSource::static_bytes(TEST_AUDIO_BYTES),
            )
            .build()
            .unwrap();

        let theme = nyaa.sound(TestSound::BattleTheme);
        assert_eq!(theme.path().unwrap(), "music/battle/theme");
        assert_eq!(
            nyaa.find_sound("music/battle/theme").unwrap().id(),
            theme.id()
        );
    }

    #[test]
    fn typed_scene_can_create_selected_or_all_placeholders() {
        let mixed = Nyaa::<TestSound>::builder_with_output(Output::new_deferred(None))
            .placeholder(TestSound::Preview)
            .sound(
                TestSound::BattleTheme,
                SoundSource::static_bytes(TEST_AUDIO_BYTES),
            )
            .build()
            .unwrap();
        assert!(matches!(
            mixed.sound(TestSound::Preview).source().unwrap(),
            SoundSource::Empty
        ));
        assert!(matches!(
            mixed.sound(TestSound::BattleTheme).source().unwrap(),
            SoundSource::StaticBytes(_)
        ));

        let placeholders = Nyaa::<TestSound>::builder_with_output(Output::new_deferred(None))
            .placeholders()
            .unwrap();
        for key in TestSound::ALL {
            assert!(matches!(
                placeholders.sound(*key).source().unwrap(),
                SoundSource::Empty
            ));
        }
    }

    #[test]
    fn required_sound_identity_survives_moves_and_cannot_be_removed_recursively() {
        let nyaa = Nyaa::<TestSound>::builder_with_output(Output::new_deferred(None))
            .sound(
                TestSound::Preview,
                SoundSource::static_bytes(TEST_AUDIO_BYTES),
            )
            .sound(
                TestSound::BattleTheme,
                SoundSource::static_bytes(TEST_AUDIO_BYTES),
            )
            .build()
            .unwrap();
        let music = nyaa.group("music").unwrap();
        let menu = nyaa.create_group("menu").unwrap();
        let theme = nyaa.sound(TestSound::BattleTheme);

        assert!(matches!(
            music.clone().remove(),
            Err(NyaaError::RequiredSound)
        ));
        assert!(nyaa.group("music/battle").is_some());
        assert!(matches!(
            theme.clone().remove(),
            Err(NyaaError::RequiredSound)
        ));

        theme.set_group(&menu).unwrap();
        assert_eq!(nyaa.sound(TestSound::BattleTheme).id(), theme.id());
        assert_eq!(theme.path().unwrap(), "menu/theme");
    }

    #[test]
    fn recursive_paths_find_sounds_and_groups() {
        let nyaa = Nyaa::new_with_output(Output::new_deferred(None));
        let music = nyaa.create_group("music").unwrap();
        let combat = music.create_group("combat").unwrap();
        let theme = combat
            .create_sound("theme", SoundSource::static_bytes(TEST_AUDIO_BYTES))
            .unwrap();

        assert_eq!(music.path().unwrap(), "music");
        assert_eq!(combat.path().unwrap(), "music/combat");
        assert_eq!(theme.path().unwrap(), "music/combat/theme");
        assert_eq!(nyaa.group("music/combat").unwrap().id(), combat.id());
        assert_eq!(
            nyaa.find_sound("music/combat/theme").unwrap().id(),
            theme.id()
        );
        assert_eq!(music.sound("combat/theme").unwrap().id(), theme.id());
    }

    #[test]
    fn names_share_one_unambiguous_sibling_namespace() {
        let nyaa = Nyaa::new_with_output(Output::new_deferred(None));
        let music = nyaa.create_group("music").unwrap();

        assert!(matches!(
            nyaa.create_sound("music", SoundSource::static_bytes(TEST_AUDIO_BYTES)),
            Err(NyaaError::DuplicateName(name)) if name == "music"
        ));
        assert!(matches!(
            music.create_group("bad/name"),
            Err(NyaaError::InvalidName)
        ));
    }

    #[test]
    fn handles_survive_unrelated_insertions_and_never_alias_reused_slots() {
        let nyaa = Nyaa::new_with_output(Output::new_deferred(None));
        let first = nyaa
            .create_sound("first", SoundSource::static_bytes(TEST_AUDIO_BYTES))
            .unwrap();
        let first_id = first.id();
        nyaa.create_sound("second", SoundSource::static_bytes(TEST_AUDIO_BYTES))
            .unwrap();

        assert_eq!(first.name().unwrap(), "first");
        first.clone().remove().unwrap();
        assert!(matches!(first.name(), Err(NyaaError::InvalidSoundHandle)));

        let replacement = nyaa
            .create_sound("replacement", SoundSource::static_bytes(TEST_AUDIO_BYTES))
            .unwrap();
        assert_ne!(replacement.id(), first_id);
        assert!(matches!(first.name(), Err(NyaaError::InvalidSoundHandle)));
    }

    #[test]
    fn reparenting_preserves_handles_and_rejects_cycles() {
        let nyaa = Nyaa::new_with_output(Output::new_deferred(None));
        let music = nyaa.create_group("music").unwrap();
        let combat = music.create_group("combat").unwrap();
        let menu = nyaa.create_group("menu").unwrap();
        let theme = combat
            .create_sound("theme", SoundSource::static_bytes(TEST_AUDIO_BYTES))
            .unwrap();

        theme.set_group(&menu).unwrap();
        assert_eq!(theme.path().unwrap(), "menu/theme");
        assert_eq!(nyaa.find_sound("menu/theme").unwrap().id(), theme.id());
        assert!(matches!(
            music.set_parent(&combat),
            Err(NyaaError::SoundGroupCycle)
        ));
    }

    #[test]
    fn removing_a_group_invalidates_all_descendant_handles() {
        let nyaa = Nyaa::new_with_output(Output::new_deferred(None));
        let music = nyaa.create_group("music").unwrap();
        let combat = music.create_group("combat").unwrap();
        let theme = combat
            .create_sound("theme", SoundSource::static_bytes(TEST_AUDIO_BYTES))
            .unwrap();

        music.remove().unwrap();

        assert!(nyaa.group("music").is_none());
        assert!(matches!(
            combat.name(),
            Err(NyaaError::InvalidSoundGroupHandle)
        ));
        assert!(matches!(theme.name(), Err(NyaaError::InvalidSoundHandle)));
    }

    #[test]
    fn local_sound_and_group_settings_remain_independent() {
        let nyaa = Nyaa::new_with_output(Output::new_deferred(None));
        let music = nyaa.create_group("music").unwrap();
        let theme = music
            .create_sound("theme", SoundSource::static_bytes(TEST_AUDIO_BYTES))
            .unwrap();

        nyaa.set_volume(0.8).unwrap();
        music.set_volume(0.5).unwrap();
        theme.set_volume(0.7).unwrap();
        theme.set_speed(1.25).unwrap();

        assert_eq!(nyaa.volume(), 0.8);
        assert_eq!(music.volume().unwrap(), 0.5);
        assert_eq!(theme.volume().unwrap(), 0.7);
        assert!((theme.effective_volume().unwrap() - 0.8 * 0.5 * 0.7).abs() < f32::EPSILON);
        assert_eq!(theme.speed().unwrap(), 1.25);

        music.set_volume(0.25).unwrap();
        assert_eq!(theme.volume().unwrap(), 0.7);
        assert!((theme.effective_volume().unwrap() - 0.8 * 0.25 * 0.7).abs() < f32::EPSILON);
        music.set_muted(true).unwrap();
        assert_eq!(theme.effective_volume().unwrap(), 0.0);
        music.set_muted(false).unwrap();
        assert!(matches!(theme.set_speed(0.0), Err(NyaaError::InvalidSpeed)));
        assert!(matches!(
            music.set_volume(f32::NAN),
            Err(NyaaError::InvalidVolume)
        ));
    }

    #[test]
    fn invalid_source_replacement_keeps_the_previous_source() {
        let nyaa = Nyaa::new_with_output(Output::new_deferred(None));
        let sound = nyaa
            .create_sound("sound", SoundSource::static_bytes(TEST_AUDIO_BYTES))
            .unwrap();
        let duration = sound.duration().unwrap();

        assert!(sound.set_source(vec![0_u8; 16]).is_err());
        assert_eq!(sound.duration().unwrap(), duration);
        assert!(matches!(
            sound.source().unwrap(),
            SoundSource::StaticBytes(bytes) if std::ptr::eq(bytes, TEST_AUDIO_BYTES)
        ));
    }

    #[test]
    fn assigning_the_same_source_preserves_the_timeline() {
        let nyaa = Nyaa::new_with_output(Output::new_deferred(None));
        let sound = nyaa
            .create_sound("sound", SoundSource::static_bytes(TEST_AUDIO_BYTES))
            .unwrap();
        let position = Duration::from_secs(42);

        sound.try_seek(position).unwrap();
        sound
            .set_source(SoundSource::static_bytes(TEST_AUDIO_BYTES))
            .unwrap();

        assert_eq!(sound.position().unwrap(), position);
    }

    #[test]
    fn seeking_after_completion_sets_the_next_play_position() {
        let nyaa = Nyaa::new();
        if !nyaa.has_output() {
            eprintln!("Skipping playback assertions: no audio output is available");
            return;
        }
        let sound = nyaa
            .create_sound("sound", SoundSource::static_bytes(TEST_AUDIO_BYTES))
            .unwrap();
        let range = Duration::from_secs(42)..Duration::from_millis(42_250);
        let sought = Duration::from_millis(42_100);

        sound.set_loop_range(range.clone()).unwrap();
        sound.play().unwrap();
        sound.wait_until_end().unwrap();
        assert_eq!(sound.playback_state().unwrap(), PlaybackState::Ended);

        sound.try_seek(sought).unwrap();
        assert_eq!(sound.playback_state().unwrap(), PlaybackState::Idle);
        sound.play().unwrap();

        let resumed = sound.position().unwrap();
        assert!(
            resumed >= sought && resumed < range.end,
            "playback resumed at {resumed:?} instead of the explicitly sought position {sought:?}"
        );
    }

    #[test]
    fn sound_transport_and_group_pause_gates_preserve_local_intent() {
        let nyaa = Nyaa::new();
        if !nyaa.has_output() {
            eprintln!("Skipping playback assertions: no audio output is available");
            return;
        }
        let music = nyaa.create_group("music").unwrap();
        let theme = music
            .create_sound("theme", SoundSource::static_bytes(TEST_AUDIO_BYTES))
            .unwrap();

        theme.play().unwrap();
        assert!(theme.is_playing().unwrap());
        assert!(matches!(
            theme.poll_event().unwrap(),
            Some(PlaybackEvent::StateChanged {
                current: PlaybackState::Playing,
                ..
            })
        ));
        assert_eq!(nyaa.poll_event().unwrap().sound, theme.id());

        music
            .set_effects(SoundEffects {
                input_gain: 0.5,
                ..SoundEffects::default()
            })
            .unwrap();
        assert!(theme.is_playing().unwrap());

        music.pause().unwrap();
        assert!(theme.is_paused().unwrap());
        theme.pause().unwrap();
        music.resume().unwrap();
        assert!(theme.is_paused().unwrap());

        theme.resume().unwrap();
        assert!(theme.is_playing().unwrap());
        theme.stop().unwrap();
        assert_eq!(theme.playback_state().unwrap(), PlaybackState::Idle);
        assert_eq!(theme.position().unwrap(), Duration::ZERO);

        theme.play().unwrap();
        assert!(theme.is_playing().unwrap());
    }

    #[test]
    fn dropping_the_root_invalidates_every_handle() {
        let sound = {
            let nyaa = Nyaa::new_with_output(Output::new_deferred(None));
            nyaa.create_sound("sound", SoundSource::static_bytes(TEST_AUDIO_BYTES))
                .unwrap()
        };

        assert!(matches!(sound.name(), Err(NyaaError::InvalidSoundHandle)));
    }
}
