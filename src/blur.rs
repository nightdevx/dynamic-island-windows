//! A Gaussian blur for composition layers, animatable as "Blur.BlurAmount" (pixels). The
//! compositor builds its effects from IGraphicsEffect descriptions; Win2D usually supplies
//! them, here it is the one effect, described by hand.

use windows::{
    core::*,
    Foundation::{IPropertyValue, PropertyValue},
    Graphics::Effects::{IGraphicsEffect, IGraphicsEffect_Impl, IGraphicsEffectSource, IGraphicsEffectSource_Impl},
    UI::Composition::{CompositionEffectFactory, CompositionEffectSourceParameter, Compositor},
    Win32::{
        Foundation::E_INVALIDARG,
        Graphics::Direct2D::{
            Common::D2D1_BORDER_MODE_SOFT, CLSID_D2D1GaussianBlur, D2D1_GAUSSIANBLUR_OPTIMIZATION_SPEED, D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION,
        },
        System::WinRT::Graphics::Direct2D::{IGraphicsEffectD2D1Interop, IGraphicsEffectD2D1Interop_Impl, GRAPHICS_EFFECT_PROPERTY_MAPPING, GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT},
    },
};

#[implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
struct Blur {
    source: IGraphicsEffectSource,
}

impl IGraphicsEffect_Impl for Blur_Impl {
    fn Name(&self) -> Result<HSTRING> {
        Ok(h!("Blur").clone())
    }

    fn SetName(&self, _: &HSTRING) -> Result<()> {
        Ok(())
    }
}

impl IGraphicsEffectSource_Impl for Blur_Impl {}

impl IGraphicsEffectD2D1Interop_Impl for Blur_Impl {
    fn GetEffectId(&self) -> Result<GUID> {
        Ok(CLSID_D2D1GaussianBlur)
    }

    fn GetNamedPropertyMapping(&self, name: &PCWSTR, index: *mut u32, mapping: *mut GRAPHICS_EFFECT_PROPERTY_MAPPING) -> Result<()> {
        if unsafe { name.to_string() }.ok().as_deref() != Some("BlurAmount") {
            return Err(E_INVALIDARG.into());
        }
        unsafe {
            *index = D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32;
            *mapping = GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT;
        }
        Ok(())
    }

    fn GetPropertyCount(&self) -> Result<u32> {
        Ok(3)
    }

    /// Standard deviation, optimization, border mode: D2D's order.
    fn GetProperty(&self, index: u32) -> Result<IPropertyValue> {
        match index {
            0 => PropertyValue::CreateSingle(0.0),
            1 => PropertyValue::CreateUInt32(D2D1_GAUSSIANBLUR_OPTIMIZATION_SPEED.0 as u32),
            2 => PropertyValue::CreateUInt32(D2D1_BORDER_MODE_SOFT.0 as u32),
            _ => Err(E_INVALIDARG.into()),
        }?
        .cast()
    }

    fn GetSource(&self, _: u32) -> Result<IGraphicsEffectSource> {
        Ok(self.source.clone())
    }

    fn GetSourceCount(&self) -> Result<u32> {
        Ok(1)
    }
}

/// Makes blur brushes. Set as a LayerVisual's effect, a brush blurs what the layer holds.
pub fn factory(compositor: &Compositor) -> Result<CompositionEffectFactory> {
    let source = CompositionEffectSourceParameter::Create(h!("source"))?.cast()?;
    let effect: IGraphicsEffect = Blur { source }.into();
    let animated = windows_collections::IIterable::<HSTRING>::from(vec![HSTRING::from("Blur.BlurAmount")]);
    compositor.CreateEffectFactoryWithProperties(&effect, &animated)
}
