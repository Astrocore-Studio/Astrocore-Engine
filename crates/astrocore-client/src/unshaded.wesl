import bevy_sprite_render::mesh2d::vertex_output::VertexOutput;

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var light_texture: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(1) var light_sampler: sampler;
@group(constants::MATERIAL_BIND_GROUP) @binding(2) var mask_texture: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(3) var mask_sampler: sampler;
@group(constants::MATERIAL_BIND_GROUP) @binding(4) var<uniform> screen: vec4<f32>;

// Светящееся: маска × видимость × (1 − свет) прибавляется к миру, уже умноженному на свет.
// Итог светящегося пикселя — его собственный цвет; вне поля зрения — ничего.
@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let light = textureSample(light_texture, light_sampler, in.uv);
    let mask = textureSample(mask_texture, mask_sampler, in.position.xy / screen.xy);
    let lift = light.a * max(vec3<f32>(1.0) - light.rgb, vec3<f32>(0.0));
    return vec4<f32>(mask.rgb * lift, 1.0);
}
