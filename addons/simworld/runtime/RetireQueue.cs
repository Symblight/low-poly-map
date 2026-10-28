using System;
using System.Collections.Generic;
using Godot;

namespace SimWorld;

/// <summary>
/// Node sets that left the cover and wait for their meshes and sprite buffers to be freed. Clearing a mesh costs the engine a free of its GPU buffers
/// (a few tens to hundreds of microseconds each); a poll_unload batch of a whole subtree, or a reconfigure, would otherwise do all of it inside one frame.
/// <see cref="Drain"/> works through the queue oldest first within a time budget. Main thread only; allocation-free in steady state.
/// </summary>
public sealed class RetireQueue<T> where T : class
{
    private readonly Queue<T> _queue = new(64);

    public int Count => _queue.Count;

    public void Enqueue(T item) => _queue.Enqueue(item);

    public bool TryDequeue(out T item) => _queue.TryDequeue(out item!);

    /// <summary>
    /// Hands queued items to <paramref name="finish"/> until <paramref name="budgetUs"/> microseconds have passed since the call; at least one item per call
    /// (a budget of 0 still makes progress). Returns the number finished.
    /// </summary>
    public int Drain(long budgetUs, Action<T> finish)
    {
        if (_queue.Count == 0) return 0;
        long t0 = (long)Time.GetTicksUsec();
        int n = 0;
        while (_queue.TryDequeue(out var item))
        {
            finish(item);
            n++;
            if ((long)Time.GetTicksUsec() - t0 >= budgetUs) break;
        }
        return n;
    }
}
