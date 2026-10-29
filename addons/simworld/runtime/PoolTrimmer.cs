#nullable enable
using System;
using System.Collections.Generic;
using Godot;

namespace SimWorld;

/// <summary>
/// Decides when a node pool gives memory back. A pool only grows to the peak of tiles held at once; after a reconfigure to a smaller area, a
/// climb-and-descend or a long flight the free list can stay far above what is needed. The rule is a low-water mark: every
/// <c>holdS</c> seconds the smallest free-list size seen in that window is compared with a margin of <c>max(minFree, peakShare x peak in use)</c>;
/// sets that stayed free the whole window above the margin are the surplus and are freed a few per frame (time-sliced, no hitch). A reconfigure storm
/// or steady flight that keeps taking sets from the free list never trips it, and the margin keeps enough free sets that a normal burst of loading
/// does not allocate again. The peak in use decays by 10 % per window, so a smaller area is eventually honoured.
/// </summary>
public sealed class PoolTrimmer
{
    private const double PeakDecayPerWindow = 0.9;
    /// <summary>A frame stops destroying sets after this many microseconds (a freed set costs 0.03 to 0.1 ms; one outlier measured 3 ms).</summary>
    private const long SliceBudgetUs = 500;

    private readonly string _name;
    private readonly int _minFree;
    private readonly double _peakShare;
    private readonly double _holdS;
    private readonly int _perFrame;

    private double _windowT;
    private int _windowMinFree = int.MaxValue, _windowMaxInUse, _peakInUse, _margin, _pending, _burstFreed;
    private long _burstUs, _burstMaxFrameUs;

    /// <summary>Sets freed since start.</summary>
    public int Trimmed { get; private set; }
    /// <summary>The free-list margin of the last closed window.</summary>
    public int Margin => _margin;

    public PoolTrimmer(string name, WorldRenderSettings s)
    {
        _name = name;
        _minFree = System.Math.Max(s.PoolTrimMinFree, 0);
        _peakShare = System.Math.Max(s.PoolTrimPeakShare, 0f);
        _holdS = s.PoolTrimHoldS;
        _perFrame = System.Math.Max(s.PoolTrimPerFrame, 1);
        _margin = _minFree;
    }

    public bool Enabled => _holdS > 0;

    /// <summary>
    /// Once per frame: runs <see cref="Update"/> and pops that many sets off <paramref name="free"/>, handing each to <paramref name="destroy"/>.
    /// Returns how many were destroyed (0 most frames). <paramref name="inUse"/> is the number of sets handed out.
    /// </summary>
    public int Step<T>(double delta, int inUse, Stack<T> free, Action<T> destroy)
    {
        int n = Update(delta, inUse, free.Count);
        if (n == 0)
        {
            if (_pending <= 0 && _burstFreed > 0) LogBurst(inUse, free.Count); // the surplus vanished mid-burst
            return 0;
        }
        long t0 = (long)Time.GetTicksUsec();
        int done = 0;
        while (done < n)
        {
            destroy(free.Pop());
            done++;
            if ((long)Time.GetTicksUsec() - t0 >= SliceBudgetUs) break; // time-sliced: the rest stays pending for the next frame
        }
        if (done < n) GiveBack(n - done);
        n = done;
        long us = (long)Time.GetTicksUsec() - t0;
        _burstUs += us;
        if (us > _burstMaxFrameUs) _burstMaxFrameUs = us;
        if (_pending <= 0) LogBurst(inUse, free.Count);
        return n;
    }

    /// <summary>
    /// Call once per frame with the number of sets in use and in the free list. Returns how many free sets the pool must destroy now (0 most frames).
    /// </summary>
    public int Update(double delta, int inUse, int free)
    {
        if (!Enabled) return 0;
        if (inUse > _windowMaxInUse) _windowMaxInUse = inUse;
        if (free < _windowMinFree) _windowMinFree = free;
        _windowT += delta;
        if (_windowT >= _holdS) CloseWindow();
        if (_pending <= 0) return 0;
        int n = System.Math.Min(System.Math.Min(_perFrame, _pending), free - _margin);
        if (n <= 0)
        {
            _pending = 0; // the free list shrank below the margin meanwhile: nothing left to give back
            return 0;
        }
        _pending -= n;
        _burstFreed += n;
        Trimmed += n;
        return n;
    }

    private void GiveBack(int k)
    {
        _pending += k;
        _burstFreed -= k;
        Trimmed -= k;
    }

    private void CloseWindow()
    {
        _peakInUse = System.Math.Max(_windowMaxInUse, (int)(_peakInUse * PeakDecayPerWindow));
        _margin = System.Math.Max(_minFree, (int)System.Math.Ceiling(_peakShare * _peakInUse));
        _pending = System.Math.Max(_windowMinFree - _margin, 0);
        _windowT = 0;
        _windowMinFree = int.MaxValue;
        _windowMaxInUse = 0;
    }

    private void LogBurst(int inUse, int freeAfter)
    {
        GD.Print($"[PoolTrim] {_name}: freed {_burstFreed} idle node sets in {_burstUs / 1000.0:0.0} ms (worst frame {_burstMaxFrameUs / 1000.0:0.00} ms); {freeAfter} stay free, {inUse} in use, "
            + $"peak {_peakInUse}, margin {_margin}, {Trimmed} trimmed in total");
        _burstFreed = 0;
        _burstUs = _burstMaxFrameUs = 0;
    }
}
