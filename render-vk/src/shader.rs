//! WGSL -> SPIR-V compilation via `naga`, done at renderer init time.
//!
//! No Vulkan SDK / glslc is assumed to be installed on the dev machine, so
//! shaders are authored in WGSL and cross-compiled in-process instead of
//! shelling out to an external compiler.

use anyhow::{anyhow, Result};
use naga::back::spv;
use naga::valid::{Capabilities, ValidationFlags, Validator};

pub fn compile_wgsl_to_spirv(source: &str) -> Result<Vec<u32>> {
    let module = naga::front::wgsl::parse_str(source)
        .map_err(|e| anyhow!("WGSL parse error: {}", e.emit_to_string(source)))?;

    let info = Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(&module)
        .map_err(|e| anyhow!("WGSL validation error: {e}"))?;

    let options = spv::Options {
        lang_version: (1, 3),
        ..spv::Options::default()
    };
    let spirv = spv::write_vec(&module, &info, &options, None)
        .map_err(|e| anyhow!("SPIR-V codegen error: {e}"))?;
    Ok(spirv)
}
