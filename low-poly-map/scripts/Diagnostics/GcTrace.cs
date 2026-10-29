using System;
using System.Collections.Generic;
using System.Diagnostics.Tracing;
using Godot;

namespace LowPolyMap.Diagnostics;

/// <summary>
/// Debug aid (<c>--gc-trace</c>): listens to the runtime's GC events in process and prints, for every collection, how its pause splits into suspending the managed threads, marking and the
/// rest (event timestamps: GCStart / GCEnd, SuspendEE begin / end, RestartEE begin / end, the mark events per root kind). It answers whether a long pause is the suspension (another thread was not at a
/// safe point), the mark (finalization queue, handles, older generations) or the sweep / compact. Events arrive on the runtime's dispatch thread, so the listener never runs inside the pause.
/// </summary>
public sealed class GcTrace : EventListener
{
    const int Gc = 0x1;   // Microsoft-Windows-DotNETRuntime GC keyword
    readonly List<string> _lines = new();
    readonly object _lock = new();
    readonly List<string> _seq = new();
    DateTime _seqStart;
    DateTime _start, _suspendBegin, _suspendEnd, _restartBegin;
    int _gen;
    string _reason = "";
    readonly Dictionary<int, long> _markBytes = new();
    readonly Dictionary<int, DateTime> _markAt = new();

    protected override void OnEventSourceCreated(EventSource source)
    {
        if (source.Name == "Microsoft-Windows-DotNETRuntime") EnableEvents(source, EventLevel.Verbose, (EventKeywords)Gc);
    }

    protected override void OnEventWritten(EventWrittenEventArgs e)
    {
        lock (_lock)
        {
            if (e.EventName != null && e.EventName.StartsWith("GCSuspendEEBegin", StringComparison.Ordinal)) { _seq.Clear(); _seqStart = e.TimeStamp; }
            _seq.Add($"{(e.TimeStamp - _seqStart).TotalMilliseconds:0.00} {e.EventName}" + (e.EventName == "GCMarkWithType" ? $"(type {e.Payload![1]}, {Convert.ToInt64(e.Payload![2])} B)" : ""));
            switch (e.EventName)
            {
                case "GCSuspendEEBegin_V1": _suspendBegin = e.TimeStamp; break;
                case "GCSuspendEEEnd_V1": _suspendEnd = e.TimeStamp; break;
                case "GCStart_V2":
                    _start = e.TimeStamp;
                    _gen = Convert.ToInt32(e.Payload![1]);
                    _reason = e.Payload![2]?.ToString() ?? "";
                    _markBytes.Clear();
                    _markAt.Clear();
                    break;
                case "GCMarkWithType":
                {
                    int type = Convert.ToInt32(e.Payload![1]);
                    _markBytes[type] = Convert.ToInt64(e.Payload![2]);
                    _markAt[type] = e.TimeStamp;
                    break;
                }
                case "GCRestartEEBegin_V1": _restartBegin = e.TimeStamp; break;
                case "GCRestartEEEnd_V1":
                {
                    double suspend = (_suspendEnd - _suspendBegin).TotalMilliseconds;
                    double work = (_restartBegin - _suspendEnd).TotalMilliseconds;
                    double restart = (e.TimeStamp - _restartBegin).TotalMilliseconds;
                    var marks = new System.Text.StringBuilder();
                    DateTime prev = _start;
                    foreach (var kv in new SortedDictionary<int, DateTime>(_markAt))
                    {
                        marks.Append(MarkName(kv.Key)).Append(' ').Append(_markBytes[kv.Key] / 1024).Append(" KB @").Append((kv.Value - _start).TotalMilliseconds.ToString("0.0")).Append(" ms; ");
                        prev = kv.Value;
                    }
                    _lines.Add($"gen{_gen} reason {_reason}: suspend {suspend:0.00} ms, gc work {work:0.00} ms (start at +{(_start - _suspendEnd).TotalMilliseconds:0.00}), restart {restart:0.00} ms | {marks} | {string.Join(", ", _seq)}");
                    break;
                }
            }
        }
    }

    static string MarkName(int type) => type switch
    {
        0 => "stack", 1 => "finalize-queue", 2 => "handles", 3 => "older-gens", 4 => "sized-ref", 5 => "overflow", 6 => "dependent-handles", _ => "type" + type,
    };

    /// <summary>Prints every collection seen so far.</summary>
    public void Print()
    {
        lock (_lock)
        {
            GD.Print($"[GcTrace] {_lines.Count} collections");
            foreach (var l in _lines) GD.Print("[GcTrace] " + l);
        }
    }
}
