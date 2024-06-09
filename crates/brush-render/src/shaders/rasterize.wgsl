#import helpers

struct Uniforms {
    // Background color behind splats.
    background: vec3f,
    // Img resolution (w, h)
    img_size: vec2u,
    // Max time dimension.
    tile_bounds: vec2u
}

@group(0) @binding(0) var<storage, read> uniforms: Uniforms;

@group(0) @binding(1) var<storage, read> depthsort_gid_from_isect: array<u32>;
@group(0) @binding(2) var<storage> compact_from_depthsort_gid: array<u32>;
@group(0) @binding(3) var<storage, read> tile_bins: array<vec2u>;
@group(0) @binding(4) var<storage, read> xys: array<vec2f>;
@group(0) @binding(5) var<storage, read> conic_comps: array<vec4f>;
@group(0) @binding(6) var<storage, read> colors: array<vec4f>;

#ifdef RASTER_U32
    @group(0) @binding(7) var<storage, read_write> out_img: array<u32>;
#else
    @group(0) @binding(7) var<storage, read_write> out_img: array<vec4f>;
    @group(0) @binding(8) var<storage, read_write> final_index : array<u32>;
#endif

var<workgroup> xy_batch: array<vec2f, helpers::TILE_SIZE>;
var<workgroup> colors_batch: array<vec4f, helpers::TILE_SIZE>;
var<workgroup> conic_comp_batch: array<vec4f, helpers::TILE_SIZE>;

// kernel function for rasterizing each tile
// each thread treats a single pixel
// each thread group uses the same gaussian data in a tile
@compute
@workgroup_size(helpers::TILE_WIDTH, helpers::TILE_WIDTH, 1)
fn main(
    @builtin(global_invocation_id) global_id: vec3u,
    @builtin(local_invocation_index) local_idx: u32,
    @builtin(workgroup_id) workgroup_id: vec3u,
) {
    let background = uniforms.background;
    let img_size = uniforms.img_size;

    // Get index of tile being drawn.
    let tile_id = workgroup_id.x + workgroup_id.y * uniforms.tile_bounds.x;
    let pix_id = global_id.x + global_id.y * img_size.x;
    let pixel_coord = vec2f(global_id.xy) + 0.5;

    // return if out of bounds
    // keep not rasterizing threads around for reading data
    let inside = global_id.x < img_size.x && global_id.y < img_size.y;

    var done = false;
    if !inside {
        // this pixel is done
        done = true;
    }

    // have all threads in tile process the same gaussians in batches
    // first collect gaussians between range.x and range.y in batches
    let range = tile_bins[tile_id];

    let num_batches = helpers::ceil_div(range.y - range.x, helpers::TILE_SIZE);
    // current visibility left to render
    var T = 1.0;

    var pix_out = vec3f(0.0);
    var final_idx = range.x;

    // collect and process batches of gaussians
    // each thread loads one gaussian at a time before rasterizing its
    // designated pixel
    for (var b = 0u; b < num_batches; b++) {
        workgroupBarrier();

        // each thread fetch 1 gaussian from front to back
        // index of gaussian to load
        let batch_start = range.x + b * helpers::TILE_SIZE;
        let isect_id = batch_start + local_idx;

        if isect_id <= range.y {
            let depthsort_gid = depthsort_gid_from_isect[isect_id];
            let compact_gid = compact_from_depthsort_gid[depthsort_gid];
            xy_batch[local_idx] = xys[compact_gid];
            conic_comp_batch[local_idx] = conic_comps[compact_gid];
            colors_batch[local_idx] = colors[compact_gid];
        }
        workgroupBarrier();

        // process gaussians in the current batch for this pixel
        let remaining = min(helpers::TILE_SIZE, range.y - batch_start);
    
        for (var t = 0u; t < remaining && !done; t++) {
            let xy = xy_batch[t];
            let conic_comp = conic_comp_batch[t];
            let color_opac = colors_batch[t];

            let conic = conic_comp.xyz;
            // TODO: Re-enable compensation.
            // let compensation = conic_comp.w;
            let opac = color_opac.w;
            let delta = xy - pixel_coord;
            let sigma = 0.5f * (conic.x * delta.x * delta.x + conic.z * delta.y * delta.y) + conic.y * delta.x * delta.y;
            let vis = exp(-sigma);
            let alpha = min(0.999f, opac * vis);

            if sigma >= 0.0 && alpha >= 1.0 / 255.0 {
                let next_T = T * (1.0 - alpha);
                if next_T <= 1e-4f { 
                    done = true;
                    break;
                }

                let fac = alpha * T;
                let c = color_opac.xyz;
                pix_out += c * fac;
                T = next_T;
                final_idx = batch_start + t;
            }
        }
    }

    if inside {
        let final_color = vec4f(pix_out + T * background, 1.0 - T);

        #ifdef RASTER_U32
            let colors_u = vec4u(clamp(final_color * 255.0, vec4f(0.0), vec4f(255.0)));
            let packed: u32 = colors_u.x | (colors_u.y << 8u) | (colors_u.z << 16u) | (colors_u.w << 24u);
            out_img[pix_id] = packed;
        #else 
            out_img[pix_id] = final_color;
            final_index[pix_id] = final_idx; 
        #endif
    }
}
