using System;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// Fixed-bucket histogram of durations in ms: <see cref="BucketMs"/> wide buckets up to <see cref="MaxMs"/> plus one
/// overflow bucket. Percentiles are nearest-rank, reported as the upper edge of the bucket that holds the rank (capped at
/// the largest value seen), so they are at most one bucket above the exact value. No allocation after construction.
/// </summary>
public sealed class DurationHistogram
{
    readonly int[] _counts;
    readonly double[] _sums;
    public double BucketMs { get; }
    public double MaxMs { get; }
    public long Count { get; private set; }
    public double Sum { get; private set; }
    public double Max { get; private set; }
    public double Min { get; private set; } = double.PositiveInfinity;

    public DurationHistogram(double bucketMs, double maxMs)
    {
        BucketMs = bucketMs;
        MaxMs = maxMs;
        int n = (int)Math.Ceiling(maxMs / bucketMs);
        _counts = new int[n + 1];   // last = overflow
        _sums = new double[n + 1];
    }

    public double Mean => Count == 0 ? 0 : Sum / Count;

    public void Add(double ms)
    {
        if (!(ms >= 0)) ms = 0;   // NaN and negatives
        int i = ms >= MaxMs ? _counts.Length - 1 : (int)(ms / BucketMs);
        if (i >= _counts.Length - 1) i = _counts.Length - 1;
        _counts[i]++;
        _sums[i] += ms;
        Count++;
        Sum += ms;
        if (ms > Max) Max = ms;
        if (ms < Min) Min = ms;
    }

    public void Reset()
    {
        Array.Clear(_counts);
        Array.Clear(_sums);
        Count = 0;
        Sum = 0;
        Max = 0;
        Min = double.PositiveInfinity;
    }

    /// <summary>Nearest-rank percentile, <paramref name="p"/> in 0..1; 0 when empty.</summary>
    public double Percentile(double p)
    {
        if (Count == 0) return 0;
        long rank = Math.Max(1, (long)Math.Ceiling(p * Count));
        long cumulative = 0;
        for (int i = 0; i < _counts.Length; i++)
        {
            cumulative += _counts[i];
            if (cumulative >= rank)
                return i == _counts.Length - 1 ? Max : Math.Min((i + 1) * BucketMs, Max);
        }
        return Max;
    }

    /// <summary>Mean of the slowest <paramref name="fraction"/> of the samples (at least one); within a partly taken bucket
    /// the bucket's own mean is used.</summary>
    public double WorstMean(double fraction)
    {
        if (Count == 0) return 0;
        long want = Math.Max(1, (long)Math.Ceiling(fraction * Count));
        long taken = 0;
        double sum = 0;
        for (int i = _counts.Length - 1; i >= 0 && taken < want; i--)
        {
            int c = _counts[i];
            if (c == 0) continue;
            long take = Math.Min(c, want - taken);
            sum += take == c ? _sums[i] : _sums[i] / c * take;
            taken += take;
        }
        return sum / taken;
    }
}
