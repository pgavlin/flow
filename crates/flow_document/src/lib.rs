#![doc = include_str!("../README.md")]

#[cfg(feature = "telemetry")]
#[doc(hidden)]
pub use tracing as __tracing;

#[cfg(feature = "render")]
pub mod cache_codec;
pub mod formats;
pub mod layout;
pub mod model;
pub mod navigation;
pub mod pagination;
pub mod position;
pub mod search;
pub mod selection;
pub mod syntax;
pub mod telemetry;
#[cfg(feature = "test-fonts")]
#[doc(hidden)]
pub mod test_fonts;
pub mod toc;

#[cfg(feature = "render")]
pub mod font_context;
#[cfg(feature = "render")]
#[doc(hidden)]
pub mod images;
#[cfg(feature = "render")]
pub mod pixels;
#[cfg(feature = "render")]
pub mod render;
#[cfg(feature = "render")]
pub mod settings;
#[cfg(feature = "render")]
#[doc(hidden)]
pub mod text;
#[cfg(feature = "render")]
pub mod theme;
#[cfg(feature = "render")]
pub mod viewer;
#[cfg(feature = "render")]
#[doc(hidden)]
pub mod viewer_cache;
