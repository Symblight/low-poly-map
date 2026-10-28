using Godot;

namespace LowPolyMap.Diagnostics;

/// <summary>Runs after every other node each frame (highest process priority value) and closes the process-phase timer that
/// <see cref="DiagnosticsNode"/> opened first, so the frame can be split into script work and the rest.</summary>
public partial class ProcessTail : Node
{
    public ProcessTail()
    {
        Name = "ProcessTail";
        ProcessMode = ProcessModeEnum.Always;
        ProcessPriority = int.MaxValue;
    }

    public override void _Process(double delta) => PerfMonitor.EndProcess();
}
