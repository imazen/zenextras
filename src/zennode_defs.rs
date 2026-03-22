//! zennode node definitions for TIFF encoding.
//!
//! Defines [`EncodeTiff`] with RIAPI-compatible querystring keys for
//! compression method and horizontal predictor control.

extern crate alloc;

use zennode::*;

/// TIFF encoding with compression and predictor options.
///
/// Supports LZW, Deflate, PackBits, or uncompressed output with
/// optional horizontal differencing predictor for improved compression.
///
/// JSON API: `{ "compression": "lzw", "predictor": true }`
/// RIAPI: `?tiff.compression=lzw&tiff.predictor=true`
#[derive(Node, Clone, Debug)]
#[node(id = "zentiff.encode", group = Encode, role = Encode)]
#[node(tags("codec", "tiff", "lossless", "encode"))]
pub struct EncodeTiff {
    /// Compression method: "none", "lzw", "deflate", or "packbits".
    ///
    /// LZW is the most widely supported lossless TIFF compression.
    /// Deflate (zlib) often achieves better ratios but has less
    /// legacy software support. PackBits is fast but weak.
    #[param(default = "lzw")]
    #[param(section = "Main", label = "Compression")]
    #[kv("tiff.compression")]
    pub compression: String,

    /// Enable horizontal differencing predictor.
    ///
    /// Stores pixel differences instead of absolute values,
    /// improving compression ratios for photographic content
    /// (typically ~35% better with LZW). Only effective with
    /// LZW or Deflate compression.
    #[param(default = true)]
    #[param(section = "Main", label = "Predictor")]
    #[kv("tiff.predictor")]
    pub predictor: bool,
}

impl Default for EncodeTiff {
    fn default() -> Self {
        Self {
            compression: alloc::string::String::from("lzw"),
            predictor: true,
        }
    }
}

/// Registration function for aggregating crates.
pub fn register(registry: &mut NodeRegistry) {
    registry.register(&ENCODE_TIFF_NODE);
}

/// All TIFF zennode definitions.
pub static ALL: &[&dyn NodeDef] = &[&ENCODE_TIFF_NODE];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_metadata() {
        let schema = ENCODE_TIFF_NODE.schema();
        assert_eq!(schema.id, "zentiff.encode");
        assert_eq!(schema.group, NodeGroup::Encode);
        assert_eq!(schema.role, NodeRole::Encode);
        assert!(schema.tags.contains(&"codec"));
        assert!(schema.tags.contains(&"tiff"));
        assert!(schema.tags.contains(&"lossless"));
        assert!(schema.tags.contains(&"encode"));
    }

    #[test]
    fn param_count_and_names() {
        let schema = ENCODE_TIFF_NODE.schema();
        let names: Vec<&str> = schema.params.iter().map(|p| p.name).collect();
        assert!(names.contains(&"compression"));
        assert!(names.contains(&"predictor"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn defaults() {
        let node = ENCODE_TIFF_NODE.create_default().unwrap();
        assert_eq!(
            node.get_param("compression"),
            Some(ParamValue::Str(alloc::string::String::from("lzw")))
        );
        assert_eq!(node.get_param("predictor"), Some(ParamValue::Bool(true)));
    }

    #[test]
    fn from_kv_compression() {
        let mut kv = KvPairs::from_querystring("tiff.compression=deflate&tiff.predictor=false");
        let node = ENCODE_TIFF_NODE.from_kv(&mut kv).unwrap().unwrap();
        assert_eq!(
            node.get_param("compression"),
            Some(ParamValue::Str(alloc::string::String::from("deflate")))
        );
        assert_eq!(node.get_param("predictor"), Some(ParamValue::Bool(false)));
        assert_eq!(kv.unconsumed().count(), 0);
    }

    #[test]
    fn from_kv_predictor_only() {
        let mut kv = KvPairs::from_querystring("tiff.predictor=false");
        let node = ENCODE_TIFF_NODE.from_kv(&mut kv).unwrap().unwrap();
        assert_eq!(node.get_param("predictor"), Some(ParamValue::Bool(false)));
        // compression should still be default
        assert_eq!(
            node.get_param("compression"),
            Some(ParamValue::Str(alloc::string::String::from("lzw")))
        );
    }

    #[test]
    fn from_kv_no_match() {
        let mut kv = KvPairs::from_querystring("w=800&h=600");
        let result = ENCODE_TIFF_NODE.from_kv(&mut kv).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn param_round_trip() {
        let mut params = ParamMap::new();
        params.insert("compression".into(), ParamValue::Str("packbits".into()));
        params.insert("predictor".into(), ParamValue::Bool(false));

        let node = ENCODE_TIFF_NODE.create(&params).unwrap();
        assert_eq!(
            node.get_param("compression"),
            Some(ParamValue::Str(alloc::string::String::from("packbits")))
        );
        assert_eq!(node.get_param("predictor"), Some(ParamValue::Bool(false)));

        // Round-trip
        let exported = node.to_params();
        let node2 = ENCODE_TIFF_NODE.create(&exported).unwrap();
        assert_eq!(
            node2.get_param("compression"),
            Some(ParamValue::Str(alloc::string::String::from("packbits")))
        );
        assert_eq!(node2.get_param("predictor"), Some(ParamValue::Bool(false)));
    }

    #[test]
    fn downcast_to_concrete() {
        let node = ENCODE_TIFF_NODE.create_default().unwrap();
        let enc = node.as_any().downcast_ref::<EncodeTiff>().unwrap();
        assert_eq!(enc.compression, "lzw");
        assert!(enc.predictor);
    }

    #[test]
    fn registry_integration() {
        let mut registry = NodeRegistry::new();
        register(&mut registry);
        assert!(registry.get("zentiff.encode").is_some());

        let result = registry.from_querystring("tiff.compression=deflate&tiff.predictor=true");
        assert_eq!(result.instances.len(), 1);
        assert_eq!(result.instances[0].schema().id, "zentiff.encode");
    }
}
