pub mod doom_fire;
pub mod metaballs;
pub mod plasma;
pub mod sampling;
pub mod screen_melt;
pub mod underwater_warp;

#[cfg(feature = "canvas")]
pub mod canvas_adapters;

pub use doom_fire::DoomFireFx;
pub use metaballs::{Metaball, MetaballsFx, MetaballsPalette, MetaballsParams};
pub use plasma::{PlasmaFx, PlasmaPalette, plasma_wave, plasma_wave_low};
pub use sampling::{
    BallState, CoordCache, FnSampler, MetaballFieldSampler, PlasmaSampler, Sampler,
    cell_to_normalized, fill_normalized_coords,
};
pub use screen_melt::ScreenMeltFx;
pub use underwater_warp::UnderwaterWarpFx;

#[cfg(feature = "canvas")]
pub use canvas_adapters::{MetaballsCanvasAdapter, PlasmaCanvasAdapter};
