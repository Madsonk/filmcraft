//! Row-parallel loops on native targets (scoped threads); sequential on `wasm32`.

/// Number of worker threads to use.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get()).clamp(1, 8)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn threads() -> usize {
    1
}

/// Calls `f(row, row_slice)` for every `row_len`-long row of `out`. `work` is a rough count of
/// multiply-adds for the whole call; small jobs stay on the calling thread.
pub(crate) fn rows<F>(out: &mut [f32], row_len: usize, work: usize, f: F)
where
    F: Fn(usize, &mut [f32]) + Sync,
{
    if row_len == 0 || out.is_empty() {
        return;
    }
    let n_rows = out.len() / row_len;
    let t = threads().min(n_rows);
    if t <= 1 || work < 2_000_000 {
        for (i, row) in out.chunks_mut(row_len).enumerate() {
            f(i, row);
        }
        return;
    }
    run_blocks(out, row_len, n_rows, t, &f);
}

#[cfg(not(target_arch = "wasm32"))]
fn run_blocks<F>(out: &mut [f32], row_len: usize, n_rows: usize, t: usize, f: &F)
where
    F: Fn(usize, &mut [f32]) + Sync,
{
    let per = n_rows.div_ceil(t);
    std::thread::scope(|s| {
        for (b, block) in out.chunks_mut(per * row_len).enumerate() {
            s.spawn(move || {
                for (i, row) in block.chunks_mut(row_len).enumerate() {
                    f(b * per + i, row);
                }
            });
        }
    });
}

#[cfg(target_arch = "wasm32")]
fn run_blocks<F>(out: &mut [f32], row_len: usize, _n_rows: usize, _t: usize, f: &F)
where
    F: Fn(usize, &mut [f32]) + Sync,
{
    for (i, row) in out.chunks_mut(row_len).enumerate() {
        f(i, row);
    }
}
