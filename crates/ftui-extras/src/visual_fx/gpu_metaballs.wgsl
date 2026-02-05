struct Params {
    width: u32,
    height: u32,
    ball_count: u32,
    _pad0: u32,
    glow: f32,
    threshold: f32,
    _pad1: vec2<f32>,
    bg_base: vec4<f32>,
    stop0: vec4<f32>,
    stop1: vec4<f32>,
    stop2: vec4<f32>,
    stop3: vec4<f32>,
};

struct Ball {
    x: f32,
    y: f32,
    r2: f32,
    hue: f32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> balls: array<Ball>;
@group(0) @binding(2) var<storage, read_write> out: array<u32>;

fn lerp_color(a: vec3<f32>, b: vec3<f32>, t: f32) -> vec3<f32> {
    return a + (b - a) * clamp(t, 0.0, 1.0);
}

fn gradient_color(t: f32) -> vec3<f32> {
    let clamped = clamp(t, 0.0, 1.0);
    let scaled = clamped * 3.0;
    let idx = min(u32(floor(scaled)), 2u);
    let local = scaled - f32(idx);
    if idx == 0u {
        return lerp_color(params.stop0.xyz, params.stop1.xyz, local);
    }
    if idx == 1u {
        return lerp_color(params.stop1.xyz, params.stop2.xyz, local);
    }
    return lerp_color(params.stop2.xyz, params.stop3.xyz, local);
}

fn pack_rgba(color: vec3<f32>, alpha: f32) -> u32 {
    // Match CPU path which truncates toward zero when casting to u8.
    let r = u32(clamp(color.r, 0.0, 1.0) * 255.0);
    let g = u32(clamp(color.g, 0.0, 1.0) * 255.0);
    let b = u32(clamp(color.b, 0.0, 1.0) * 255.0);
    let a = u32(clamp(alpha, 0.0, 1.0) * 255.0);
    return (r << 24u) | (g << 16u) | (b << 8u) | a;
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }
    let width = f32(params.width);
    let height = f32(params.height);
    if (width == 0.0 || height == 0.0) {
        return;
    }

    let idx = gid.y * params.width + gid.x;
    // Sample at cell centers to match CPU's normalized coordinate cache.
    let nx = (f32(gid.x) + 0.5) / width;
    let ny = (f32(gid.y) + 0.5) / height;

    var sum = 0.0;
    var weighted_hue = 0.0;
    var total_weight = 0.0;
    let eps = 0.0001;

    for (var i = 0u; i < params.ball_count; i = i + 1u) {
        let ball = balls[i];
        let dx = nx - ball.x;
        let dy = ny - ball.y;
        let dist_sq = dx * dx + dy * dy;
        if (dist_sq > eps) {
            let contrib = ball.r2 / dist_sq;
            sum = sum + contrib;
            weighted_hue = weighted_hue + ball.hue * contrib;
            total_weight = total_weight + contrib;
        } else {
            sum = sum + 100.0;
            weighted_hue = weighted_hue + ball.hue * 100.0;
            total_weight = total_weight + 100.0;
        }
    }

    if (sum > params.glow) {
        var avg_hue = 0.0;
        if (total_weight > 0.0) {
            avg_hue = weighted_hue / total_weight;
        }

        let denom = max(params.threshold - params.glow, 0.0001);
        let t = (sum - params.glow) / denom;
        let smoothed = t * t * (3.0 - 2.0 * t);
        let intensity = select(smoothed, 1.0, sum > params.threshold);

        let base = gradient_color(avg_hue);
        let blended = lerp_color(params.bg_base.xyz, base, intensity);
        out[idx] = pack_rgba(blended, 1.0);
    } else {
        out[idx] = 0u;
    }
}
