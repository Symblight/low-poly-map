using System;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// A slowly sampled number (once a second): min, max, last, mean and the least-squares slope per minute, kept as running sums, so
/// a soak of any length needs no storage. NaN samples (a value the library does not report) are ignored.
/// </summary>
public sealed class Series
{
    long _count;
    double _sum, _t0, _sumT, _sumV, _sumTV, _sumTT;

    public double Min { get; private set; } = double.PositiveInfinity;
    public double Max { get; private set; } = double.NegativeInfinity;
    public double Last { get; private set; }
    public long Count => _count;
    public double Mean => _count == 0 ? 0 : _sum / _count;

    /// <summary>Least-squares slope of the value over time, per minute; 0 with fewer than three samples.</summary>
    public double SlopePerMinute
    {
        get
        {
            if (_count < 3) return 0;
            double denominator = _count * _sumTT - _sumT * _sumT;
            return denominator <= 0 ? 0 : (_count * _sumTV - _sumT * _sumV) / denominator * 60.0;
        }
    }

    /// <summary>Adds the value seen <paramref name="seconds"/> after the start of the run.</summary>
    public void Add(double seconds, double value)
    {
        if (double.IsNaN(value)) return;
        if (_count == 0) _t0 = seconds;
        double t = seconds - _t0;   // centred on the first sample: the sums stay small
        _count++;
        _sum += value;
        _sumT += t;
        _sumV += value;
        _sumTV += t * value;
        _sumTT += t * t;
        if (value < Min) Min = value;
        if (value > Max) Max = value;
        Last = value;
    }

    public void Reset()
    {
        _count = 0;
        _sum = _t0 = _sumT = _sumV = _sumTV = _sumTT = 0;
        Min = double.PositiveInfinity;
        Max = double.NegativeInfinity;
        Last = 0;
    }

    /// <summary>{"min":…,"max":…,"last":…,"mean":…,"slope_per_min":…,"samples":…} as the object <paramref name="key"/>.</summary>
    public void Write(JsonText j, string key)
    {
        j.BeginObject(key);
        j.Field("min", _count == 0 ? 0 : Math.Round(Min, 3));
        j.Field("max", _count == 0 ? 0 : Math.Round(Max, 3));
        j.Field("last", Math.Round(Last, 3));
        j.Field("mean", Math.Round(Mean, 3));
        j.Field("slope_per_min", Math.Round(SlopePerMinute, 4));
        j.Field("samples", _count);
        j.End();
    }
}
