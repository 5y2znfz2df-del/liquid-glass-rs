// 液态玻璃番茄钟 —— 安装程序
//
// 单文件：payload.zip 作为嵌入资源被打进 exe，运行时直接解压到目标目录，
// 不经过 %TEMP%，所以不会被「创建临时目录」这类问题卡住。
//
// 用法：
//   LiquidGlassPomodoro-Setup.exe                          图形界面安装（可以选目录）
//   LiquidGlassPomodoro-Setup.exe --silent                 静默安装，自动挑一个可写目录
//   LiquidGlassPomodoro-Setup.exe --silent --dir D:\Apps   静默安装到指定目录
//
// 编译见 installer/build-installer.ps1

using System;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.IO.Compression;
using System.Reflection;
using Microsoft.Win32;
using System.Runtime.InteropServices;
using System.Threading;
using System.Windows.Forms;

// 版本信息：没有这些，文件属性里 Company/Product 都是空的、版本显示 0.0.0.0，
// 双击时"未知发布者"会显得更像可疑程序。填上之后资源管理器里能看到产品名和公司名。
[assembly: AssemblyTitle("液态玻璃番茄钟 安装程序")]
[assembly: AssemblyDescription("liquid-glass-rs / 液态玻璃番茄钟 安装程序")]
[assembly: AssemblyProduct("liquid-glass-rs · 液态玻璃番茄钟")]
[assembly: AssemblyCompany("XiangFu")]
[assembly: AssemblyCopyright("GPL-3.0-or-later © 2026 XiangFu")]
[assembly: AssemblyTrademark("")]
[assembly: AssemblyCulture("")]
[assembly: AssemblyVersion("0.4.0.0")]
[assembly: AssemblyFileVersion("0.4.0.0")]
[assembly: AssemblyInformationalVersion("0.4.0")]

namespace LiquidGlassSetup
{
    internal static class Program
    {
        // winexe 没有自己的控制台，--silent 时挂到父进程的控制台上才能看到输出
        [DllImport("kernel32.dll")]
        private static extern bool AttachConsole(int processId);

        private const string AppName = "液态玻璃番茄钟";
        private const string AppKey = "LiquidGlassPomodoro";
        private const string AppVersion = "0.4.0";
        private const string ResourceName = "payload.zip";

        private static string _destDir = "";
        private static string _targetExe = "";

        private static Label _status;
        private static ProgressBar _bar;
        private static TextBox _pathBox;
        private static Button _installButton;
        private static Button _browseButton;

        // ------------------------------------------------------------------ 入口 --
        [STAThread]
        private static int Main(string[] args)
        {
            bool silent = HasFlag(args, "--silent");
            string requested = GetOption(args, "--dir");

            if (silent)
            {
                AttachConsole(-1);
                return RunSilent(requested);
            }

            Application.EnableVisualStyles();
            Form form = BuildWindow(requested);
            Application.Run(form);
            return 0;
        }

        private static int RunSilent(string requested)
        {
            try
            {
                if (!string.IsNullOrWhiteSpace(requested))
                {
                    UseInstallDir(Path.GetFullPath(requested));
                    if (!CanWrite(_destDir))
                        throw new UnauthorizedAccessException("指定的目录写不进去：" + _destDir);
                }
                else
                {
                    UseInstallDir(PickWritableCandidate());
                }

                InstallCore(delegate (string text, int percent)
                {
                    string line = string.Format("[{0,3}%] {1}", percent, text);
                    Console.WriteLine(line);
                    TryLog(line);
                });

                Console.WriteLine("安装完成：" + _targetExe);
                TryLog("安装完成：" + _targetExe);
                return 0;
            }
            catch (Exception ex)
            {
                TryLog(ex.ToString());
                Console.Error.WriteLine("安装失败：" + ex.Message);
                return 1;
            }
        }

        // -------------------------------------------------------------- 界面 --
        private static Form BuildWindow(string requested)
        {
            var form = new Form
            {
                Text = AppName + " 安装程序",
                ClientSize = new Size(460, 232),
                FormBorderStyle = FormBorderStyle.FixedDialog,
                StartPosition = FormStartPosition.CenterScreen,
                MaximizeBox = false,
                MinimizeBox = false,
                BackColor = Color.FromArgb(28, 28, 30),
                ForeColor = Color.White,
                Font = new Font("Microsoft YaHei UI", 9f)
            };

            var title = new Label
            {
                Text = "选择安装位置",
                Bounds = new Rectangle(20, 16, 420, 22),
                ForeColor = Color.FromArgb(245, 245, 247),
                Font = new Font("Microsoft YaHei UI", 10.5f, FontStyle.Bold)
            };

            _pathBox = new TextBox
            {
                Bounds = new Rectangle(20, 46, 330, 24),
                Text = "",
                ReadOnly = true,
                BackColor = Color.FromArgb(44, 44, 46),
                ForeColor = Color.FromArgb(245, 245, 247),
                BorderStyle = BorderStyle.FixedSingle
            };

            _browseButton = new Button
            {
                Text = "浏览…",
                Bounds = new Rectangle(358, 45, 82, 26)
            };
            _browseButton.Click += OnBrowse;

            var hint = new Label
            {
                Text = "默认会挑一个当前能写入的目录；也可以自己选（比如 D:\\Apps\\LiquidGlassPomodoro）。",
                Bounds = new Rectangle(20, 78, 420, 34),
                ForeColor = Color.FromArgb(150, 150, 155)
            };

            _status = new Label
            {
                Text = "准备就绪",
                Bounds = new Rectangle(20, 116, 420, 20),
                ForeColor = Color.FromArgb(180, 180, 185)
            };

            _bar = new ProgressBar
            {
                Bounds = new Rectangle(20, 138, 420, 14),
                Minimum = 0,
                Maximum = 100,
                Visible = false
            };

            _installButton = new Button
            {
                Text = "安装",
                Bounds = new Rectangle(238, 176, 110, 32),
                DialogResult = DialogResult.None
            };
            _installButton.Click += OnInstall;

            var cancel = new Button
            {
                Text = "取消",
                Bounds = new Rectangle(358, 176, 82, 32)
            };
            cancel.Click += delegate { form.Close(); };

            form.Controls.AddRange(new Control[]
            {
                title, _pathBox, _browseButton, hint, _status, _bar, _installButton, cancel
            });

            // 首选指定目录，其次自动挑一个可写的
            string initial = !string.IsNullOrWhiteSpace(requested)
                ? Path.GetFullPath(requested)
                : SafePickWritable();

            _pathBox.Text = initial;
            form.AcceptButton = _installButton;
            return form;
        }

        private static string SafePickWritable()
        {
            try { return PickWritableCandidate(); }
            catch (Exception ex)
            {
                TryLog(ex.Message);
                return CandidateDirs().Length > 0 ? CandidateDirs()[0] : "";
            }
        }

        private static void OnBrowse(object sender, EventArgs e)
        {
            using (var dialog = new FolderBrowserDialog())
            {
                dialog.Description = "选择安装目录（建议用一个空文件夹，例如 D:\\Apps\\LiquidGlassPomodoro）";
                dialog.ShowNewFolderButton = true;
                string current = _pathBox.Text.Trim();
                if (current.Length > 0)
                    dialog.SelectedPath = current;

                if (dialog.ShowDialog() != DialogResult.OK)
                    return;

                // 用户可能选中了上级目录，自动补一个 Pomodoro 子目录，避免把文件倒进 D:\
                string picked = dialog.SelectedPath;
                if (string.Equals(Path.GetFileName(picked.TrimEnd('\\')), "LiquidGlassPomodoro",
                                  StringComparison.OrdinalIgnoreCase))
                {
                    _pathBox.Text = picked;
                }
                else
                {
                    _pathBox.Text = Path.Combine(picked, "LiquidGlassPomodoro");
                }
            }
        }

        private static void OnInstall(object sender, EventArgs e)
        {
            string target = _pathBox.Text.Trim();
            if (target.Length == 0)
            {
                MessageBox.Show("请先选择安装位置。", AppName, MessageBoxButtons.OK, MessageBoxIcon.Warning);
                return;
            }

            UseInstallDir(target);
            if (!CanWrite(_destDir))
            {
                MessageBox.Show(
                    "这个目录写不进去：\r\n" + _destDir +
                    "\r\n\r\n换一个目录试试（比如 D:\\Apps\\LiquidGlassPomodoro 或桌面上的文件夹）。",
                    AppName, MessageBoxButtons.OK, MessageBoxIcon.Warning);
                return;
            }

            _installButton.Enabled = false;
            _browseButton.Enabled = false;
            _pathBox.Enabled = false;
            _bar.Visible = true;

            try
            {
                InstallCore(delegate (string text, int percent)
                {
                    _status.Text = text;
                    _bar.Value = Math.Max(0, Math.Min(100, percent));
                    _status.Refresh();
                    _bar.Refresh();
                });

                _status.Text = "安装完成，正在启动…";
                _status.Refresh();
                Thread.Sleep(400);
                CloseOwner();
            }
            catch (Exception ex)
            {
                TryLog(ex.ToString());
                _bar.Visible = false;
                _installButton.Enabled = true;
                _browseButton.Enabled = true;
                _pathBox.Enabled = true;
                _status.Text = "安装失败";
                MessageBox.Show("安装失败：\r\n" + ex.Message, AppName + " 安装程序",
                                MessageBoxButtons.OK, MessageBoxIcon.Error);
            }
        }

        private static void CloseOwner()
        {
            foreach (Form form in Application.OpenForms)
            {
                form.Close();
                return;
            }
        }

        // ------------------------------------------------------------ 目录选择 --
        private static string[] CandidateDirs()
        {
            string local = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
            string roaming = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
            string profile = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);

            var list = new System.Collections.Generic.List<string>
            {
                Path.Combine(local, "Programs", "LiquidGlassPomodoro"),
                Path.Combine(local, "LiquidGlassPomodoro"),
                Path.Combine(roaming, "LiquidGlassPomodoro"),
                Path.Combine(profile, "LiquidGlassPomodoro"),
            };

            try
            {
                string here = Path.GetDirectoryName(Assembly.GetExecutingAssembly().Location);
                if (!string.IsNullOrEmpty(here))
                    list.Add(Path.Combine(here, "LiquidGlassApp"));
            }
            catch (Exception)
            {
                // 拿不到 exe 路径就算了
            }
            return list.ToArray();
        }

        /// <summary>按候选顺序找第一个真正能写入的目录。</summary>
        private static string PickWritableCandidate()
        {
            var failures = new System.Text.StringBuilder();
            foreach (string candidate in CandidateDirs())
            {
                if (CanWrite(candidate))
                {
                    if (failures.Length > 0)
                        TryLog("以下目录不可写，已跳过：\r\n" + failures);
                    return candidate;
                }
                failures.AppendLine("  ✗ " + candidate);
            }
            throw new UnauthorizedAccessException(
                "这些目录都写不进去：\r\n" + failures +
                "\r\n可以点「浏览…」换一个位置，或右键以管理员身份运行。");
        }

        private static void UseInstallDir(string dir)
        {
            _destDir = Path.GetFullPath(dir).TrimEnd(Path.DirectorySeparatorChar);
            _targetExe = Path.Combine(_destDir, "LiquidGlassPomodoro.exe");
        }

        /// <summary>真的建一次目录并写一个临时文件，确认有写权限。</summary>
        private static bool CanWrite(string dir)
        {
            try
            {
                Directory.CreateDirectory(dir);
                string probe = Path.Combine(dir, ".write-probe");
                File.WriteAllText(probe, "ok");
                File.Delete(probe);
                return true;
            }
            catch (Exception)
            {
                return false;
            }
        }

        // -------------------------------------------------------------- 安装 --
        /// <summary>安装主体：解压、建快捷方式、写卸载器、启动程序。</summary>
        private static void InstallCore(Action<string, int> report)
        {
            report("正在安装到 " + _destDir, 3);

            // 覆盖安装时旧进程可能还开着，先关掉，否则文件被占用会装失败
            KillRunningApp(report);

            if (Directory.Exists(_destDir))
            {
                report("检测到旧版本，正在移除…", 5);
                DeleteDirWithRetry();
            }

            Directory.CreateDirectory(_destDir);
            ExtractPayload(report);

            // 解压完先自检：少了任何一项都说明包不完整，直接报清楚缺什么
            report("正在检查运行时文件…", 94);
            VerifyInstall();

            report("正在创建快捷方式…", 96);
            try
            {
                CreateShortcuts();
                WriteUninstaller();
                WriteUninstallRegistry();
            }
            catch (Exception ex)
            {
                TryLog("创建快捷方式失败（不影响程序本身）：" + ex.Message);
                report("快捷方式创建失败，但程序已安装", 96);
            }

            report("安装完成，正在启动…", 100);
            Thread.Sleep(300);

            // 启动失败不代表安装失败：文件已经就位了
            try
            {
                StartApp();
            }
            catch (Exception ex)
            {
                TryLog("自动启动失败（不影响安装）：" + ex.Message);
                report("已安装，但自动启动失败，请手动打开：" + _targetExe, 100);
                Thread.Sleep(1500);
            }
        }

        /// <summary>关掉正在运行的番茄钟（升级安装时文件会被占用）。</summary>
        private static void KillRunningApp(Action<string, int> report)
        {
            try
            {
                // 关掉正在运行的旧版本
                var victims = new System.Collections.Generic.List<Process>();
                foreach (string name in new[] { "LiquidGlassPomodoro" })
                {
                    foreach (Process p in Process.GetProcessesByName(name))
                    {
                        // 只动装在我们目录里的进程
                        string exePath = null;
                        try { exePath = p.MainModule.FileName; }
                        catch (Exception) { }

                        bool ours = exePath == null
                            ? name == "LiquidGlassPomodoro"
                            : exePath.StartsWith(_destDir, StringComparison.OrdinalIgnoreCase);

                        if (ours) victims.Add(p);
                    }
                }

                if (victims.Count == 0) return;

                report("检测到程序正在运行，正在关闭…", 4);
                foreach (Process p in victims)
                {
                    try { p.Kill(); } catch (Exception) { }
                }

                // 等进程真正退出：Kill() 只是发出请求，句柄释放还要一点时间
                var deadline = DateTime.Now.AddSeconds(6);
                while (DateTime.Now < deadline && AliveCount() > 0)
                    Thread.Sleep(200);

                Thread.Sleep(300);
            }
            catch (Exception ex)
            {
                TryLog("关闭旧进程失败（继续安装）：" + ex.Message);
            }
        }

        private static int AliveCount()
        {
            int count = 0;
            foreach (string name in new[] { "LiquidGlassPomodoro" })
            {
                foreach (Process p in Process.GetProcessesByName(name))
                {
                    try
                    {
                        string exePath = p.MainModule.FileName;
                        if (exePath.StartsWith(_destDir, StringComparison.OrdinalIgnoreCase))
                            count++;
                    }
                    catch (Exception)
                    {
                        if (name == "LiquidGlassPomodoro") count++;
                    }
                }
            }
            return count;
        }

        /// <summary>删目录，文件被占用时重试几次。</summary>
        private static void DeleteDirWithRetry()
        {
            for (int attempt = 0; attempt < 4; attempt++)
            {
                try
                {
                    if (!Directory.Exists(_destDir)) return;
                    Directory.Delete(_destDir, true);
                    return;
                }
                catch (Exception)
                {
                    Thread.Sleep(500);
                }
            }
        }

        /// <summary>确认关键文件都在，缺了直接报名字。</summary>
        private static void VerifyInstall()
        {
            string[] required =
            {
                "LiquidGlassPomodoro.exe",
                "README.md",
            };

            var missing = new System.Collections.Generic.List<string>();
            foreach (string rel in required)
            {
                if (!File.Exists(Path.Combine(_destDir, rel)))
                    missing.Add(rel);
            }

            if (missing.Count > 0)
            {
                string detail = string.Join("\r\n  ", missing.ToArray());
                TryLog("安装包不完整，缺少：\r\n  " + detail);
                throw new InvalidOperationException(
                    "安装包不完整，缺少这些文件：\r\n  " + detail +
                    "\r\n\r\n可能是下载不完整，或被安全软件删掉了。请重新下载安装包。");
            }

            // Windows 版本只影响毛玻璃效果，不影响运行，所以只记日志
            Version v = Environment.OSVersion.Version;
            if (v.Major < 10)
                TryLog("当前系统是 Windows " + v.Major + "." + v.Minor + "，亚克力模糊不可用，界面会退回不透明样式。");
        }

        private static void ExtractPayload(Action<string, int> report)
        {
            using (Stream stream = Assembly.GetExecutingAssembly().GetManifestResourceStream(ResourceName))
            {
                if (stream == null)
                    throw new InvalidOperationException("安装包损坏：找不到 " + ResourceName);

                using (var zip = new ZipArchive(stream, ZipArchiveMode.Read))
                {
                    int total = zip.Entries.Count;
                    int done = 0;

                    foreach (ZipArchiveEntry entry in zip.Entries)
                    {
                        // zip 顶层是 LiquidGlassPomodoro/，剥掉这一段直接解到 _destDir，
                        // 这样目标目录叫什么名字都能装
                        string relative = entry.FullName.Replace('/', Path.DirectorySeparatorChar);
                        int cut = relative.IndexOf(Path.DirectorySeparatorChar);
                        relative = cut >= 0 ? relative.Substring(cut + 1) : "";
                        if (relative.Length == 0) { done++; continue; }

                        string target = Path.Combine(_destDir, relative);

                        if (entry.Name.Length == 0)
                        {
                            Directory.CreateDirectory(target);
                        }
                        else
                        {
                            string dir = Path.GetDirectoryName(target);
                            if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);
                            entry.ExtractToFile(target, true);
                        }

                        done++;
                        if (done % 50 == 0 || done == total)
                            report(string.Format("正在解压… {0}/{1}", done, total),
                                   8 + (int)(86.0 * done / Math.Max(1, total)));
                    }
                }
            }

            if (!File.Exists(_targetExe))
                throw new InvalidOperationException("解压后没有找到 LiquidGlassPomodoro.exe");
        }

        // ----------------------------------------------------------- 快捷方式 --
        private static void CreateShortcuts()
        {
            Type shellType = Type.GetTypeFromProgID("WScript.Shell");
            if (shellType == null) return;
            object shell = Activator.CreateInstance(shellType);

            string desktop = Environment.GetFolderPath(Environment.SpecialFolder.Desktop);
            MakeLink(shell, Path.Combine(desktop, AppName + ".lnk"), _targetExe, _destDir, _targetExe + ",0");

            string startMenu = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.Programs), AppName);
            Directory.CreateDirectory(startMenu);
            MakeLink(shell, Path.Combine(startMenu, AppName + ".lnk"), _targetExe, _destDir, _targetExe + ",0");

            string uninstall = Path.Combine(_destDir, "uninstall.cmd");
            MakeLink(shell, Path.Combine(startMenu, "卸载.lnk"), uninstall, _destDir,
                     Path.Combine(Environment.SystemDirectory, "shell32.dll") + ",131");
        }

        private static void MakeLink(object shell, string linkPath, string target,
                                     string workDir, string iconLocation)
        {
            object shortcut = shell.GetType().InvokeMember(
                "CreateShortcut", BindingFlags.InvokeMethod, null, shell, new object[] { linkPath });
            Type type = shortcut.GetType();

            type.InvokeMember("TargetPath", BindingFlags.SetProperty, null, shortcut, new object[] { target });
            type.InvokeMember("WorkingDirectory", BindingFlags.SetProperty, null, shortcut, new object[] { workDir });
            type.InvokeMember("IconLocation", BindingFlags.SetProperty, null, shortcut, new object[] { iconLocation });
            type.InvokeMember("Description", BindingFlags.SetProperty, null, shortcut, new object[] { AppName });
            type.InvokeMember("Save", BindingFlags.InvokeMethod, null, shortcut, null);
        }

        /// <summary>写注册表卸载项，这样"设置 → 应用"里能看到它并点卸载。</summary>
        private static void WriteUninstallRegistry()
        {
            try
            {
                using (RegistryKey k = Registry.CurrentUser.CreateSubKey(
                    @"Software\Microsoft\Windows\CurrentVersion\Uninstall\" + AppKey))
                {
                    if (k == null) return;
                    long size = 0;
                    try { size = new FileInfo(_targetExe).Length / 1024; } catch (Exception) { }
                    string cmd = Path.Combine(_destDir, "uninstall.cmd");
                    k.SetValue("DisplayName", AppName, RegistryValueKind.String);
                    k.SetValue("DisplayVersion", AppVersion, RegistryValueKind.String);
                    k.SetValue("Publisher", "XiangFu", RegistryValueKind.String);
                    k.SetValue("InstallLocation", _destDir, RegistryValueKind.String);
                    k.SetValue("DisplayIcon", _targetExe + ",0", RegistryValueKind.String);
                    k.SetValue("UninstallString", "\"" + cmd + "\"", RegistryValueKind.String);
                    k.SetValue("NoModify", 1, RegistryValueKind.DWord);
                    k.SetValue("NoRepair", 1, RegistryValueKind.DWord);
                    if (size > 0) k.SetValue("EstimatedSize", (int)size, RegistryValueKind.DWord);
                }
            }
            catch (Exception ex)
            {
                TryLog("写卸载注册表项失败（不影响使用）：" + ex.Message);
            }
        }

        private static void WriteUninstaller()
        {
            // 卸载脚本里的目录必须和实际安装位置一致
            string ps1 = Path.Combine(_destDir, "uninstall.ps1");
            File.WriteAllText(ps1,
                "$ErrorActionPreference = 'SilentlyContinue'\r\n" +
                "$dest = '" + _destDir.Replace("'", "''") + "'\r\n" +
                "$desktop = Join-Path ([Environment]::GetFolderPath('Desktop')) '" + AppName + ".lnk'\r\n" +
                "$startMenu = Join-Path ([Environment]::GetFolderPath('Programs')) '" + AppName + "'\r\n" +
                "Get-Process -Name LiquidGlassPomodoro | Stop-Process -Force\r\n" +
                "Start-Sleep -Milliseconds 400\r\n" +
                "Remove-Item $desktop -Force\r\n" +
                "Remove-Item $startMenu -Recurse -Force\r\n" +
                "Start-Sleep -Milliseconds 300\r\n" +
                "Remove-Item 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\LiquidGlassPomodoro' -Recurse -Force\r\n" +
                "Start-Sleep -Milliseconds 200\r\n" +
                "Start-Process -WindowStyle Hidden -FilePath 'cmd.exe' -ArgumentList '/c,' + [char]34 + 'timeout /t 2 >nul & rmdir /s /q ' + [char]34 + [char]34 + $dest + [char]34 + [char]34 + [char]34\r\n",
                new System.Text.UTF8Encoding(true));

            string cmd = Path.Combine(_destDir, "uninstall.cmd");
            File.WriteAllText(cmd,
                "@echo off\r\n" +
                "powershell -NoProfile -ExecutionPolicy Bypass -File \"%~dp0uninstall.ps1\"\r\n");
        }

        private static void StartApp()
        {
            Process.Start(new ProcessStartInfo
            {
                FileName = _targetExe,
                WorkingDirectory = _destDir,
                UseShellExecute = true
            });
        }

        // -------------------------------------------------------------- 杂项 --
        private static bool HasFlag(string[] args, string flag)
        {
            foreach (string a in args)
                if (string.Equals(a, flag, StringComparison.OrdinalIgnoreCase)) return true;
            return false;
        }

        private static string GetOption(string[] args, string name)
        {
            for (int i = 0; i < args.Length - 1; i++)
                if (string.Equals(args[i], name, StringComparison.OrdinalIgnoreCase))
                    return args[i + 1];
            return null;
        }

        /// <summary>把详细异常写进日志。用户目录写不了就退到 exe 旁边、再退到 TEMP。</summary>
        private static void TryLog(string text)
        {
            string body = "[" + DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss") + "]\r\n" + text + "\r\n\r\n";
            var targets = new System.Collections.Generic.List<string>();
            try
            {
                targets.Add(Path.Combine(
                    Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                    "LiquidGlassPomodoro-Setup.log"));
            }
            catch (Exception) { }
            try
            {
                targets.Add(Path.Combine(
                    Path.GetDirectoryName(Assembly.GetExecutingAssembly().Location) ?? ".",
                    "LiquidGlassPomodoro-Setup.log"));
            }
            catch (Exception) { }
            try { targets.Add(Path.Combine(Path.GetTempPath(), "LiquidGlassPomodoro-Setup.log")); }
            catch (Exception) { }

            foreach (string path in targets)
            {
                try
                {
                    File.AppendAllText(path, body);
                    return;
                }
                catch (Exception)
                {
                    // 这个位置写不了，试下一个
                }
            }
        }
    }
}
