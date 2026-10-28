using System.Collections.Generic;
using Godot;

namespace SimWorld;

/// <summary>
/// Logs a problem once per key and counts every occurrence, so a repeating fault (a tile the library delivered but the host cannot draw) neither
/// floods the console nor goes unnoticed. The key set is bounded: past <see cref="MaxKeys"/> new keys are counted but not logged again.
/// </summary>
public sealed class OnceLog
{
    private const int MaxKeys = 512;
    private readonly HashSet<string> _seen = new();

    /// <summary>Every occurrence, logged or not.</summary>
    public int Count { get; private set; }

    /// <summary>Distinct keys that were logged.</summary>
    public int Distinct => _seen.Count;

    /// <summary>Pushes an error the first time <paramref name="key"/> is seen; later calls only count.</summary>
    public void Error(string key, string message)
    {
        Count++;
        if (_seen.Count >= MaxKeys || !_seen.Add(key)) return;
        GD.PushError(message);
    }

    /// <summary>Pushes a warning the first time <paramref name="key"/> is seen; later calls only count.</summary>
    public void Warn(string key, string message)
    {
        Count++;
        if (_seen.Count >= MaxKeys || !_seen.Add(key)) return;
        GD.PushWarning(message);
    }

    public void Clear()
    {
        _seen.Clear();
        Count = 0;
    }
}
