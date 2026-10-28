// H0 spike (throwaway): CDLOD grid render path feasibility on Godot Mobile / Metal. Delete scripts/spike, shaders/spike, scenes/spike.
using Godot;
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Threading.Tasks;

namespace LowPolyMap;

public partial class GridSpike : Node
{
	const int N = 64;            // quads per tile edge
	const int V = N + 1;         // vertices per edge
	const int T = N + 3;         // texture size 67
	const int Layers = 256;
	const int Tiles = 300;
	const int Cols = 20;
	const float TileSize = 512f;

	string _shaderCode = "";
	SubViewport _tvp = null!;    // test viewport 1024x1024
	SubViewport _pvp = null!;    // perf viewport 1920x1080
	Camera3D _pcam = null!;
	Node3D _pRoot = null!;
	ArrayMesh _unitGrid = null!;
	ArrayMesh _baselineMesh = null!;

	enum Fmt { R16, RF, RH }

	static void P(string s) => GD.Print("SPIKE " + s);

	async Task Frames(int n) { for (int i = 0; i < n; i++) await ToSignal(GetTree(), SceneTree.SignalName.ProcessFrame); }

	public override async void _Ready()
	{
		try { await Run(); }
		catch (Exception e) { GD.PrintErr("SPIKE EXCEPTION " + e); }
		GetTree().Quit();
	}

	// ---------- stats ----------
	static string St(List<double> v, string unit = "")
	{
		if (v.Count == 0) return "n/a";
		var s = v.OrderBy(x => x).ToList();
		return $"med {s[s.Count / 2]:F3} p95 {s[(int)(s.Count * 0.95)]:F3} max {s[^1]:F3}{unit}";
	}

	// ---------- textures ----------
	static int Q(int layer, int x, int y)
	{
		int i = x - 1, j = y - 1;
		if (layer == 255) return unchecked(i * 977 + j * 131 + layer * 17 + i * j * 3) & 0xFFFF;
		if (layer == 3) return Math.Clamp(Math.Max(i, 0) * 1000 + Math.Max(j, 0) * 7, 0, 65535);
		return (int)(32768 + 28000 * Math.Sin(i * 0.15 + layer * 0.7) * Math.Cos(j * 0.11 + layer * 0.3));
	}

	static Image.Format ImgFmt(Fmt f) => f switch { Fmt.R16 => Image.Format.R16, Fmt.RF => Image.Format.Rf, _ => Image.Format.Rh };
	static int Bpp(Fmt f) => f == Fmt.RF ? 4 : 2;

	static Image MakeLayer(Fmt f, int layer, int salt = 0)
	{
		var b = new byte[T * T * Bpp(f)];
		int k = 0;
		for (int y = 0; y < T; y++)
			for (int x = 0; x < T; x++)
			{
				int q = layer == 255 || layer == 3 ? Q(layer, x, y) : Math.Clamp(Q(layer, x, y) + salt, 0, 65535);
				if (f == Fmt.R16) { b[k++] = (byte)(q & 255); b[k++] = (byte)(q >> 8); }
				else if (f == Fmt.RF) { var fb = BitConverter.GetBytes(q / 65535f); b[k++] = fb[0]; b[k++] = fb[1]; b[k++] = fb[2]; b[k++] = fb[3]; }
				else { ushort hb = BitConverter.HalfToUInt16Bits((Half)(q / 65535f)); b[k++] = (byte)(hb & 255); b[k++] = (byte)(hb >> 8); }
			}
		return Image.CreateFromData(T, T, false, ImgFmt(f), b);
	}

	static Texture2DArray MakeTexture(Fmt f, out double createMs, out double imagesMs)
	{
		var sw = Stopwatch.StartNew();
		var arr = new Godot.Collections.Array<Image>();
		for (int l = 0; l < Layers; l++) arr.Add(MakeLayer(f, l));
		imagesMs = sw.Elapsed.TotalMilliseconds;
		sw.Restart();
		var tex = new Texture2DArray();
		var err = tex.CreateFromImages(arr);
		createMs = sw.Elapsed.TotalMilliseconds;
		if (err != Error.Ok) P($"CreateFromImages {f} error {err}");
		return tex;
	}

	ShaderMaterial MakeMat(string defines, Texture2DArray tex)
	{
		var sh = new Shader { Code = defines + "\n" + _shaderCode };
		var m = new ShaderMaterial { Shader = sh };
		m.SetShaderParameter("heights", tex);
		return m;
	}

	// ---------- meshes ----------
	// Builds `count` grids. tileXform(t) gives (origin x, origin z, size) of tile t.
	static ArrayMesh BuildGridPacked(int count, Func<int, Vector3> tileXform, Func<int, float[]>? custom0, Func<int, float[]>? custom1)
	{
		int vc = V * V;
		var verts = new Vector3[count * vc];
		var uvs = new Vector2[count * vc];
		var idx = new int[count * N * N * 6];
		float[]? c0 = custom0 != null ? new float[count * vc * 4] : null;
		float[]? c1 = custom1 != null ? new float[count * vc * 4] : null;
		int ii = 0;
		for (int t = 0; t < count; t++)
		{
			var o = tileXform(t);
			float[]? t0 = custom0?.Invoke(t), t1 = custom1?.Invoke(t);
			int bv = t * vc;
			for (int j = 0; j < V; j++)
				for (int i = 0; i < V; i++)
				{
					int v = bv + j * V + i;
					verts[v] = new Vector3(o.X + i / (float)N * o.Z, 0, o.Y + j / (float)N * o.Z);
					uvs[v] = new Vector2(i, j);
					if (c0 != null) Array.Copy(t0!, 0, c0, v * 4, 4);
					if (c1 != null) Array.Copy(t1!, 0, c1, v * 4, 4);
				}
			for (int j = 0; j < N; j++)
				for (int i = 0; i < N; i++)
				{
					int v00 = bv + j * V + i, v10 = v00 + 1, v01 = v00 + V, v11 = v01 + 1;
					idx[ii++] = v00; idx[ii++] = v10; idx[ii++] = v01;
					idx[ii++] = v10; idx[ii++] = v11; idx[ii++] = v01;
				}
		}
		var arrays = new Godot.Collections.Array();
		arrays.Resize((int)Mesh.ArrayType.Max);
		arrays[(int)Mesh.ArrayType.Vertex] = verts;
		arrays[(int)Mesh.ArrayType.TexUV] = uvs;
		arrays[(int)Mesh.ArrayType.Index] = idx;
		long flags = 0;
		if (c0 != null) { arrays[(int)Mesh.ArrayType.Custom0] = c0; flags |= (long)Mesh.ArrayCustomFormat.RgbaFloat << (int)Mesh.ArrayFormat.FormatCustom0Shift; }
		if (c1 != null) { arrays[(int)Mesh.ArrayType.Custom1] = c1; flags |= (long)Mesh.ArrayCustomFormat.RgbaFloat << (int)Mesh.ArrayFormat.FormatCustom1Shift; }
		var mesh = new ArrayMesh();
		mesh.AddSurfaceFromArrays(Mesh.PrimitiveType.Triangles, arrays, null, null, (Mesh.ArrayFormat)flags);
		return mesh;
	}

	// ---------- viewports ----------
	SubViewport NewViewport(int w, int h)
	{
		var vp = new SubViewport
		{
			Size = new Vector2I(w, h),
			OwnWorld3D = true,
			RenderTargetUpdateMode = SubViewport.UpdateMode.Always,
			Msaa3D = Viewport.Msaa.Disabled,
			TransparentBg = false,
		};
		AddChild(vp);
		return vp;
	}

	static byte[] Pixels(SubViewport vp, out int stride, out int w, out int h)
	{
		var img = vp.GetTexture().GetImage();
		if (img.GetFormat() != Image.Format.Rgba8 && img.GetFormat() != Image.Format.Rgb8) img.Convert(Image.Format.Rgba8);
		stride = img.GetFormat() == Image.Format.Rgba8 ? 4 : 3;
		w = img.GetWidth(); h = img.GetHeight();
		return img.GetData();
	}

	static double SrgbToLin(double c) => c <= 0.04045 ? c / 12.92 : Math.Pow((c + 0.055) / 1.055, 2.4);

	// ---------- test pass (correctness) ----------
	MultiMeshInstance3D? _testInst;

	Task<byte[]> TestPass(string defines, Texture2DArray tex, int layer, bool useBuffer, int checkCustom, int checkColor, float expectOffset, out int stride)
	{
		_testInst?.QueueFree();
		var mm = new MultiMesh { TransformFormat = MultiMesh.TransformFormatEnum.Transform3D, UseColors = true, UseCustomData = true, Mesh = _unitGrid, InstanceCount = 1 };
		var cust = new Color(layer, 2000.123f, -1234.5678f, 0.1234567f);
		var col = new Color(0.123456789f, 1234.5f, 3.14159274f, 1f);
		if (useBuffer)
		{
			mm.SetBuffer(new float[] { 64, 0, 0, 0, 0, 1, 0, 0, 0, 0, 64, 0, col.R, col.G, col.B, col.A, cust.R, cust.G, cust.B, cust.A });
		}
		else
		{
			mm.SetInstanceTransform(0, new Transform3D(Basis.FromScale(new Vector3(64, 1, 64)), Vector3.Zero));
			mm.SetInstanceColor(0, col);
			mm.SetInstanceCustomData(0, cust);
		}
		var mat = MakeMat(defines, tex);
		mat.SetShaderParameter("check_custom", checkCustom);
		mat.SetShaderParameter("check_color", checkColor);
		mat.SetShaderParameter("expect_offset", expectOffset);
		_testInst = new MultiMeshInstance3D { Multimesh = mm, MaterialOverride = mat, CustomAabb = new Aabb(new Vector3(0, -10, 0), new Vector3(64, 20, 64)) };
		_tvp.AddChild(_testInst);
		return GrabTest(out stride);
	}

	async Task<byte[]> GrabAsync() { await Frames(4); return Pixels(_tvp, out _, out _, out _); }
	Task<byte[]> GrabTest(out int stride)
	{
		stride = 4; // Pixels() decides; recomputed by caller via TestStride
		return GrabAsync();
	}
	int TestStride() { Pixels(_tvp, out int s, out _, out _); return s; }

	static (double r, double g, double b) MaxRgbLin(byte[] px, int stride, bool lin)
	{
		int mr = 0, mg = 0, mb = 0;
		for (int p = 0; p < px.Length; p += stride) { mr = Math.Max(mr, px[p]); mg = Math.Max(mg, px[p + 1]); mb = Math.Max(mb, px[p + 2]); }
		double f(int v) => lin ? SrgbToLin(v / 255.0) : v / 255.0;
		return (f(mr), f(mg), f(mb));
	}

	async Task Correctness()
	{
		P("=== 2/3 CORRECTNESS (vertex texelFetch, INSTANCE_CUSTOM / COLOR precision) ===");
		_tvp = NewViewport(1024, 1024);
		var cam = new Camera3D { Projection = Camera3D.ProjectionType.Orthogonal, Size = 64, Near = 0.1f, Far = 500 };
		_tvp.AddChild(cam);
		cam.LookAtFromPosition(new Vector3(32, 100, 32), new Vector3(32, 0, 32), new Vector3(0, 0, -1));
		RenderingServer.SetDefaultClearColor(Colors.Black);

		// sanity: is output sRGB-encoded? ramp test on R16
		var texR16 = MakeTexture(Fmt.R16, out _, out _);
		var pxb = await TestPass("#define TEST_RAMP", texR16, 3, false, 0, 0, 0f, out _);
		int stride = TestStride();
		P($"ramp image stride {stride}");
		double worstLin = 0, worstRaw = 0; int n = 0;
		var sb = new System.Text.StringBuilder();
		foreach (int py in new[] { 100, 500, 900 })
			foreach (int px in new[] { 40, 300, 520, 777, 1000 })
			{
				double xf = (px + 0.5) / 16.0, yf = (py + 0.5) / 16.0;
				double exp = (xf * 1000 + yf * 7) / 65535.0;
				int o = (py * 1024 + px) * stride;
				double raw = pxb[o] / 255.0;
				double lin = SrgbToLin(raw);
				worstLin = Math.Max(worstLin, Math.Abs(lin - exp));
				worstRaw = Math.Max(worstRaw, Math.Abs(raw - exp));
				if (n++ < 4) sb.Append($" [{px},{py} exp {exp:F4} raw {raw:F4} lin {lin:F4}]");
			}
		P($"ramp readback R16 samples:{sb}");
		bool linear = worstRaw < worstLin;
		P($"ramp worst abs error vs expected: assuming sRGB-encoded output {worstLin:F4}, assuming linear output {worstRaw:F4} (8-bit step 0.0039) -> output is {(linear ? "LINEAR" : "sRGB-encoded")}; texelFetch ramp {(Math.Min(worstLin, worstRaw) < 0.006 ? "CORRECT" : "WRONG")}");

		// exactness per format (error in 1/65535 units, max over image)
		foreach (Fmt f in Enum.GetValues<Fmt>())
		{
			var tex = f == Fmt.R16 ? texR16 : MakeTexture(f, out _, out _);
			var px = await TestPass("#define TEST_EXACT", tex, 255, false, 0, 0, 0f, out _);
			var m = MaxRgbLin(px, stride, !linear);
			// negative control: expect_offset of 3 q-units must show ~3
			var px2 = await TestPass("#define TEST_EXACT", tex, 255, false, 0, 0, 3f / 65535f, out _);
			var m2 = MaxRgbLin(px2, stride, !linear);
			P($"texelFetch exact {f}: max |h - expected| = {m.r * 255:F2} q-units (1/65535); negative control (+3 q offset) reads {m2.r * 255:F2}");
		}

		// custom / colour precision, both setter paths
		foreach (bool buf in new[] { false, true })
		{
			var sb2 = new System.Text.StringBuilder();
			string[] cn = { "custom.y=2000.123", "custom.z=-1234.5678", "custom.w=0.1234567", "NEGCTRL custom.y vs 2000.5" };
			string[] kn = { "color.r=0.123456789", "color.g=1234.5", "color.b=3.14159274", "NEGCTRL color.r vs 0.5" };
			for (int k = 0; k < 4; k++)
			{
				var px = await TestPass("#define TEST_EXACT", texR16, 255, buf, k, k, 0f, out _);
				var m = MaxRgbLin(px, stride, false);
				sb2.Append($"\n    {cn[k]}: {(m.g > 0.5 ? "MISMATCH" : "exact")}   {kn[k]}: {(m.b > 0.5 ? "MISMATCH" : "exact")}");
			}
			P($"instance data via {(buf ? "SetBuffer" : "SetInstance* setters")}:{sb2}");
		}
		_testInst?.QueueFree();
		_tvp.QueueFree();
		await Frames(2);
	}

	// ---------- perf ----------
	record Sample(List<double> Gpu, List<double> Cpu, List<double> Frame);

	async Task<Sample> Measure(int warm, int n, Action<int>? perFrame = null)
	{
		var rid = _pvp.GetViewportRid();
		RenderingServer.ViewportSetMeasureRenderTime(rid, true);
		await Frames(warm);
		var s = new Sample(new(), new(), new());
		ulong last = Time.GetTicksUsec();
		for (int i = 0; i < n; i++)
		{
			perFrame?.Invoke(i);
			await Frames(1);
			ulong now = Time.GetTicksUsec();
			s.Frame.Add((now - last) / 1000.0); last = now;
			s.Gpu.Add(RenderingServer.ViewportGetMeasuredRenderTimeGpu(rid));
			s.Cpu.Add(RenderingServer.ViewportGetMeasuredRenderTimeCpu(rid));
		}
		return s;
	}

	void ClearPerf() { foreach (var c in _pRoot.GetChildren().ToList()) { if (c is Camera3D) continue; c.QueueFree(); } }

	static float[] InstCustom(int t) => new float[] { t % 250, 0f, 400f, 0f };
	static float[] InstColor(int t) => new float[] { (t * 7) % 16, 0f, 0f, 1f };

	Vector3 TileXf(int t) => new Vector3((t % Cols) * TileSize, (t / Cols) * TileSize, TileSize);

	void Populate(string cfg, ShaderMaterial mat, ShaderMaterial baseMat, int copies = 1)
	{
		ClearPerf();
		for (int cp = 0; cp < copies; cp++)
		switch (cfg)
		{
			case "baseline":
				_pRoot.AddChild(new MeshInstance3D { Mesh = _baselineMesh, MaterialOverride = baseMat, CustomAabb = new Aabb(new Vector3(-10, -10, -10), new Vector3(Cols * TileSize + 20, 600, 15 * TileSize + 20)) });
				break;
			case "mm1":
				_pRoot.AddChild(MakeMm(0, Tiles, mat));
				break;
			case "mm5":
				for (int g = 0; g < 5; g++) _pRoot.AddChild(MakeMm(g * 60, 60, mat));
				break;
			case "mm5-noaabb":
				for (int g = 0; g < 5; g++) _pRoot.AddChild(MakeMm(g * 60, 60, mat, false));
				break;
		}
	}

	MultiMeshInstance3D MakeMm(int first, int count, ShaderMaterial mat, bool aabb = true)
	{
		var mm = new MultiMesh { TransformFormat = MultiMesh.TransformFormatEnum.Transform3D, UseColors = true, UseCustomData = true, Mesh = _unitGrid, InstanceCount = count };
		for (int k = 0; k < count; k++)
		{
			int t = first + k;
			var o = TileXf(t);
			mm.SetInstanceTransform(k, new Transform3D(Basis.FromScale(new Vector3(o.Z, 1, o.Z)), new Vector3(o.X, 0, o.Y)));
			var c = InstColor(t); var d = InstCustom(t);
			mm.SetInstanceColor(k, new Color(c[0], c[1], c[2], c[3]));
			mm.SetInstanceCustomData(k, new Color(d[0], d[1], d[2], d[3]));
		}
		var inst = new MultiMeshInstance3D { Multimesh = mm, MaterialOverride = mat };
		if (aabb)
		{
			int r0 = first / Cols, r1 = (first + count - 1) / Cols;
			inst.CustomAabb = new Aabb(new Vector3(0, -10, r0 * TileSize), new Vector3(Cols * TileSize, 600, (r1 - r0 + 1) * TileSize));
		}
		return inst;
	}

	async Task Run()
	{
		_shaderCode = FileAccess.GetFileAsString("res://shaders/spike/grid_spike.gdshader");
		DisplayServer.WindowSetVsyncMode(DisplayServer.VSyncMode.Disabled);
		Engine.MaxFps = 0;
		OS.LowProcessorUsageMode = false;
		await Frames(3);
		P($"vsync mode now {DisplayServer.WindowGetVsyncMode()}, window {DisplayServer.WindowGetSize()}, max_fps {Engine.MaxFps}");
		RenderingServer.GlobalShaderParameterSet("simworld_curve_inv_2r", 1.0f / (2 * 6371000f));
		RenderingServer.GlobalShaderParameterSet("simworld_curve_center", new Vector3(Cols * TileSize / 2, 0, 7 * TileSize));
		P($"Godot {Engine.GetVersionInfo()["string"]}  adapter '{RenderingServer.GetVideoAdapterName()}' api '{RenderingServer.GetVideoAdapterApiVersion()}' method '{RenderingServer.GetCurrentRenderingMethod()}' driver '{RenderingServer.GetCurrentRenderingDriverName()}'");
		var rd = RenderingServer.GetRenderingDevice();
		if (rd != null) P($"RD max array layers? limits: texture_array_layers {rd.LimitGet(RenderingDevice.Limit.MaxTextureArrayLayers)}, vertex_input_attribs {rd.LimitGet(RenderingDevice.Limit.MaxVertexInputAttributes)}, samplers/stage {rd.LimitGet(RenderingDevice.Limit.MaxSamplersPerShaderStage)}");

		_unitGrid = BuildGridPacked(1, _ => new Vector3(0, 0, 1), null, null);
		_unitGrid.CustomAabb = new Aabb(new Vector3(0, -2000, 0), new Vector3(1, 6000, 1));

		P("=== 1 TEXTURE ARRAY CREATE (256 layers 67x67) ===");
		var texs = new Dictionary<Fmt, Texture2DArray>();
		foreach (Fmt f in Enum.GetValues<Fmt>())
		{
			var cm = new List<double>(); var im = new List<double>();
			Texture2DArray t = null!;
			for (int r = 0; r < 4; r++) { t = MakeTexture(f, out double c, out double i); cm.Add(c); im.Add(i); }
			texs[f] = t;
			P($"create {f}: Image build 256 layers (CPU, test-side) {im[1]:F1} ms; CreateFromImages {string.Join(" / ", cm.Select(x => x.ToString("F2")))} ms (4 runs, first includes warmup); size {256 * T * T * Bpp(f) / 1024.0:F0} KiB");
		}

		await Correctness();

		// perf viewport
		_pvp = NewViewport(1920, 1080);
		_pRoot = new Node3D(); _pvp.AddChild(_pRoot);
		_pcam = new Camera3D { Fov = 75, Near = 10, Far = 60000 };
		_pRoot.AddChild(_pcam);
		_pcam.LookAtFromPosition(new Vector3(Cols * TileSize / 2, 6000, 7.5f * TileSize + 1200), new Vector3(Cols * TileSize / 2, 0, 7.5f * TileSize), Vector3.Up);
		_pcam.Current = true;

		_baselineMesh = BuildGridPacked(Tiles, TileXf, t => InstCustom(t), t => InstColor(t));
		_baselineMesh.CustomAabb = new Aabb(new Vector3(-10, -10, -10), new Vector3(Cols * TileSize + 20, 600, 15 * TileSize + 20));
		var matMm = MakeMat("", texs[Fmt.R16]);
		var matBase = MakeMat("#define BASELINE", texs[Fmt.R16]);
		P($"=== 4 PERF: {Tiles} tiles x {N}x{N} quads = {Tiles * N * N * 2} tris, 1920x1080 SubViewport, vsync off ===");
		P("(GPU = RenderingServer.ViewportGetMeasuredRenderTimeGpu on the 1080p viewport; Frame = main loop interval)");
		var results = new Dictionary<string, Sample>();
		foreach (var cfg in new[] { "empty", "baseline", "mm1", "mm5", "mm5-noaabb" })
		{
			var meds = new List<double>();
			foreach (int k in cfg == "empty" ? new[] { 1 } : new[] { 1, 8, 16, 32 })
			{
				if (cfg == "empty") ClearPerf(); else Populate(cfg, matMm, matBase, k);
				var s = await Measure(30, 120);
				results[cfg + k] = s;
				meds.Add(s.Frame.OrderBy(x => x).ElementAt(s.Frame.Count / 2));
				P($"{cfg,-11} x{k}: frame {St(s.Frame, " ms")} | CPU render {St(s.Cpu, " ms")} | GPU {St(s.Gpu, " ms")}");
			}
			if (meds.Count > 1) P($"{cfg,-11} frame-time slope per copy (x16->x32)/16 = {(meds[3] - meds[2]) / 16:F3} ms; (x8->x16)/8 = {(meds[2] - meds[1]) / 8:F3} ms; (x8->x32)/24 = {(meds[3] - meds[1]) / 24:F3} ms");
		}
		// screenshot sanity of the mm5 path
		Populate("mm5", matMm, matBase, 1);
		await Frames(5);
		var shot = _pvp.GetTexture().GetImage();
		shot.SavePng(ProjectSettings.GlobalizePath("res://shots/spike_grid.png"));
		var px = Pixels(_pvp, out int st, out int w, out int h);
		long nonBlack = 0; for (int p = 0; p < px.Length; p += st) if (px[p] + px[p + 1] + px[p + 2] > 0) nonBlack++;
		P($"mm5 coverage: {100.0 * nonBlack / (w * h):F1} % of pixels non-black (saved res://shots/spike_grid.png)");

		P("=== 1b UpdateLayer cost + frame impact (under the mm5 load, 1080p) ===");
		Populate("mm5", matMm, matBase, 10); // GPU-bound (about 10 scene copies) so a stall shows in the frame time
		foreach (Fmt f in Enum.GetValues<Fmt>())
		{
			var tex = texs[f];
			matMm.SetShaderParameter("heights", tex);
			var imgs = new Image[32];
			for (int i = 0; i < imgs.Length; i++) imgs[i] = MakeLayer(f, 10 + i, i * 50);
			foreach (int k in new[] { 0, 1, 8 })
			{
				int frames = k == 1 ? 500 : 200;
				var costs = new List<double>();
				int ctr = 0;
				var s = await Measure(20, frames, _ =>
				{
					for (int u = 0; u < k; u++)
					{
						var img = imgs[ctr % imgs.Length]; int layer = (ctr * 7 + 5) % 250; ctr++;
						long t0 = Stopwatch.GetTimestamp();
						tex.UpdateLayer(img, layer);
						costs.Add((Stopwatch.GetTimestamp() - t0) * 1000.0 / Stopwatch.Frequency);
					}
				});
				string c = k == 0 ? "" : $" | UpdateLayer call {St(costs, " ms")} ({costs.Count} calls)";
				P($"{f} {k,2} upd/frame: GPU {St(s.Gpu, " ms")} | frame {St(s.Frame, " ms")}{c}");
			}
		}
		P("DONE");
	}
}
