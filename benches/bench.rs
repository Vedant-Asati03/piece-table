use criterion::{
    BatchSize, BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use pbtree::PieceTable;
use std::hint::black_box;
use std::time::Duration;

const SIZES: [usize; 5] = [100_000, 500_000, 1_000_000, 2_000_000, 3_000_000];

fn build_table(base_size: usize, edit_count: usize) -> PieceTable<char> {
    let mut table = PieceTable::new(vec!['a'; base_size]);
    for i in 0..edit_count {
        let pos = table.len() / 2;
        let payload = [char::from_u32(('a' as u32) + (i as u32 % 26)).unwrap(); 8];
        table.insert(pos, &payload);
    }
    table
}

fn benchmark_insert_middle(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert_middle");
    group.sampling_mode(SamplingMode::Flat);
    for &size in &SIZES {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &doc_size| {
            b.iter_batched(
                || PieceTable::new(vec!['a'; doc_size]),
                |mut table| {
                    let pos = table.len() / 2;
                    table.insert(pos, &['x'; 64]);
                    black_box(table.len());
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

fn benchmark_delete_middle(c: &mut Criterion) {
    let mut group = c.benchmark_group("delete_middle");
    group.sampling_mode(SamplingMode::Flat);
    for &size in &SIZES {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &doc_size| {
            b.iter_batched(
                || build_table(doc_size, 1_000),
                |mut table| {
                    let pos = table.len() / 3;
                    table.delete(pos, 128);
                    black_box(table.len());
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

fn benchmark_find(c: &mut Criterion) {
    let mut group = c.benchmark_group("find_randomish");
    group.sampling_mode(SamplingMode::Flat);
    for &size in &SIZES {
        let table = build_table(size, 3_000);
        let max = table.len().max(1);

        group.throughput(Throughput::Elements(max as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, _| {
            let mut index = 0usize;
            b.iter(|| {
                index = (index + 7_919) % max;
                black_box(table.get(index));
            })
        });
    }
    group.finish();
}

fn benchmark_mixed_edit_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("mixed_edit_workload");
    group.sampling_mode(SamplingMode::Flat);
    for &size in &SIZES {
        group.throughput(Throughput::Elements(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &doc_size| {
            b.iter_batched(
                || build_table(doc_size, 1_000),
                |mut table| {
                    for i in 0..100 {
                        let mid = table.len() / 2;
                        let ch = char::from_u32(('a' as u32) + (i as u32 % 26)).unwrap();
                        table.insert(mid, &[ch; 16]);
                        let del_pos = table.len() / 3;
                        table.delete(del_pos, 8);
                        black_box(table.get(table.len() / 4));
                    }
                    black_box(table.len());
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

fn criterion_config() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_secs(5))
        .measurement_time(Duration::from_secs(12))
        .sample_size(50)
        .noise_threshold(0.03)
}

criterion_group!(
    name = benches;
    config = criterion_config();
    targets =
    benchmark_insert_middle,
    benchmark_delete_middle,
    benchmark_find,
    benchmark_mixed_edit_workload
);
criterion_main!(benches);
