use rodio::cpal::traits::DeviceTrait;

use crate::Backend;

/// An individual output device such as speakers, headphones, or a virtual device.
///
/// Values are obtained from [`crate::Output::available_devices`] or
/// [`crate::Output::available_devices_for_backend`] and can be passed to
/// [`crate::Output::try_new_with_device`] or [`crate::Nyaa::switch_device`].
///
/// Devices are identified by their CPAL device id when the platform provides one and fall back
/// to name matching otherwise. A device obtained from enumeration may no longer exist when it is
/// opened, in which case opening returns [`crate::OutputError::DeviceNotFound`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub(crate) backend: Backend,
    pub(crate) id: Option<rodio::cpal::DeviceId>,
    pub(crate) description: rodio::cpal::DeviceDescription,
    pub(crate) is_default: bool,
}

impl std::fmt::Display for Device {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.description)?;
        Ok(())
    }
}

impl Device {
    /// Returns the backend that provides this device.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// Returns the stable device id when the platform provides one.
    pub fn id(&self) -> Option<&rodio::cpal::DeviceId> {
        self.id.as_ref()
    }

    /// Returns the structured CPAL description of this device.
    ///
    /// The description exposes the human-readable name plus manufacturer, device type
    /// (speaker, headphones, headset, virtual, ...), and interface type where the platform
    /// reports them.
    pub fn description(&self) -> &rodio::cpal::DeviceDescription {
        &self.description
    }

    /// Returns the human-readable device name.
    pub fn name(&self) -> &str {
        self.description.name()
    }

    /// Returns whether this device is the default output device of its backend.
    pub fn is_default(&self) -> bool {
        self.is_default
    }

    /// Returns the device type categorization (speaker, headphones, virtual, ...).
    pub fn device_type(&self) -> rodio::cpal::DeviceType {
        self.description.device_type()
    }

    /// Returns the interface/connection type (built-in, USB, Bluetooth, virtual, ...).
    pub fn interface_type(&self) -> rodio::cpal::InterfaceType {
        self.description.interface_type()
    }

    /// Creates a [`Device`] from a CPAL device.
    pub(crate) fn from_cpal_device(
        backend: Backend,
        device: &rodio::cpal::Device,
        is_default: bool,
    ) -> Option<Self> {
        let description = device.description().ok()?;
        let id = device.id().ok();

        Some(Self {
            backend,
            id,
            description,
            is_default,
        })
    }

    /// Returns whether this device's CPAL device matches the given CPAL device.
    pub(crate) fn matches_cpal_device(&self, device: &rodio::cpal::Device) -> bool {
        if let (Some(wanted), Ok(actual)) = (self.id.as_ref(), device.id())
            && wanted == &actual
        {
            return true;
        }

        device
            .description()
            .is_ok_and(|description| description.name() == self.name())
    }
}
