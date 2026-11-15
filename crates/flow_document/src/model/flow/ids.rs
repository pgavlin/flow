use std::fmt;

macro_rules! local_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[cfg_attr(feature = "render", derive(serde::Serialize, serde::Deserialize))]
        #[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u32);

        impl $name {
            #[allow(dead_code)]
            pub(in crate::model::flow) const fn from_index(index: u32) -> Self {
                Self(index)
            }

            #[allow(dead_code)]
            pub(in crate::model::flow) const fn index(self) -> usize {
                self.0 as usize
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_tuple(stringify!($name)).field(&self.0).finish()
            }
        }
    };
}

local_id!(
    NodeId,
    "A node identity valid only within its issuing flow document."
);

impl NodeId {
    pub(crate) const fn layout_index(self) -> usize {
        self.0 as usize
    }
}
local_id!(
    StyleInputId,
    "A style-input identity valid only within its issuing flow document."
);
local_id!(
    ComputedStyleId,
    "A computed-style identity valid only within its issuing style snapshot."
);
local_id!(
    OriginId,
    "An origin identity valid only within its issuing flow document."
);
local_id!(
    ResourceId,
    "A resource identity valid only within its issuing flow document."
);
local_id!(
    SourceArtifactId,
    "A source-artifact identity valid only within its issuing flow document."
);

impl StyleInputId {
    /// The complete default style input present in every flow document.
    pub const DEFAULT: Self = Self(0);
}
