using System.Text;
using Godot;
using SimWorld;

namespace LowPolyMap;

/// <summary>
/// Launch arg <c>--window-selftest</c> (night windows W4): scrubs the hour 12 -> 27 h (= 12:00 -> 03:00) through <see cref="WorldView.SetTime"/>
/// and checks the lit-window shares written to both building materials. Needs a world profile with lit_share tables (api 1.0).
/// Checks: shares come from the profile; the plain and the dithering material hold the same array; ids 3, 5, 6, 7 are always 0; at whole
/// hours the value equals the table entry; from 18 h to 27 h no profile's share ever rises (windows only turn off, acceptance 3);
/// the array is uploaded only when the hour moved by more than 0.01 h (also across the 24 h wrap). Prints PASS/FAIL, exit code 0/1.
/// </summary>
public static class WindowSelfTest
{
    private const string Arg = "--window-selftest";

    public static bool Requested()
    {
        foreach (var a in OS.GetCmdlineUserArgs()) if (a == Arg) return true;
        return false;
    }

    public static bool Run(WorldView view)
    {
        var fails = new StringBuilder();
        void Fail(string s) { if (fails.Length < 1500) fails.AppendLine("  " + s); }

        var wp = WindowProfile.FromWorldProfile(view.Streamer.IsAvailable ? view.Streamer.WorldProfile() : new Godot.Collections.Dictionary());
        if (wp == null || !view.WindowSharesFromProfile)
        {
            GD.Print("WINDOW SELFTEST FAIL: the world profile has no lights.profiles.*.windows.lit_share tables (stale library?), shares are the built-in fallback");
            return false;
        }

        var prev = new float[WindowProfile.ProfileCount];
        var cur = new float[WindowProfile.ProfileCount];
        int steps = 0;
        for (int i = 0; i <= 300; i++) // 12.00 .. 27.00 h in 0.05 h steps
        {
            float hour = 12f + i * 0.05f;
            view.SetTime(hour % 24f, 1f);
            var plain = view.ReadMaterialWindowShares(false);
            var fade = view.ReadMaterialWindowShares(true);
            steps++;
            if (plain.Length != 8 || fade.Length != 8) { Fail($"hour {hour:0.00}: window_share has {plain.Length} / {fade.Length} entries"); break; }
            wp.SharesAt(hour % 24f, cur);
            for (int id = 0; id < 8; id++)
            {
                if (plain[id] != fade[id]) Fail($"hour {hour:0.00} id {id}: plain material {plain[id]} != dithering material {fade[id]}");
                if (Mathf.Abs(plain[id] - cur[id]) > 1e-6f) Fail($"hour {hour:0.00} id {id}: material {plain[id]} != table {cur[id]}");
                if ((id == 3 || id == 5 || id == 6 || id == 7) && plain[id] != 0f) Fail($"hour {hour:0.00}: id {id} has share {plain[id]} (must be 0)");
                if (plain[id] < 0f || plain[id] > 1f) Fail($"hour {hour:0.00} id {id}: share {plain[id]} outside 0..1");
                if (hour > 18.01f && plain[id] > prev[id] + 1e-6f) Fail($"hour {hour:0.00} id {id}: share rises {prev[id]} -> {plain[id]} between 18:00 and 03:00");
            }
            System.Array.Copy(plain, prev, 8);
        }

        // Whole hours equal the table entries (the tables are what the profile says, not a re-derived curve).
        foreach (int h in new[] { 0, 6, 12, 18, 21, 23 })
        {
            view.SetTime(h, 1f);
            var p = view.ReadMaterialWindowShares(false);
            for (int id = 0; id < 8; id++)
            {
                float want = wp.LitShare[id]?[h] ?? 0f;
                if (Mathf.Abs(p[id] - want) > 1e-6f) Fail($"hour {h}: id {id} share {p[id]} != table entry {want}");
            }
        }

        // Upload guard: same hour, and moves of 0.004 h, do not upload; a move of 0.02 h does; the wrap 23.995 -> 0.0 is 0.005 h.
        view.SetTime(10f, 1f);
        int u0 = view.WindowShareUploads;
        for (int i = 0; i < 100; i++) view.SetTime(10f, 1f);
        view.SetTime(10.004f, 1f);
        view.SetTime(10.009f, 1f);
        if (view.WindowShareUploads != u0) Fail($"uploads grew by {view.WindowShareUploads - u0} for hour moves <= 0.009 h");
        view.SetTime(10.02f, 1f);
        if (view.WindowShareUploads != u0 + 1) Fail($"a 0.02 h move gave {view.WindowShareUploads - u0} uploads, expected 1");
        view.SetTime(23.995f, 1f);
        int u1 = view.WindowShareUploads;
        view.SetTime(0.0f, 1f);
        if (view.WindowShareUploads != u1) Fail("23.995 -> 0.0 (0.005 h across the wrap) uploaded again");

        bool ok = fails.Length == 0;
        GD.Print(ok ? $"WINDOW SELFTEST PASS: {steps} hours scrubbed 12:00 -> 03:00, shares match the profile tables on both materials, never rise after 18:00, uploads only on hour moves > 0.01 h"
                    : "WINDOW SELFTEST FAIL:\n" + fails);
        return ok;
    }
}
