#!/bin/sh
# Builds a trivial .NET app and a startup hook, then runs the real-hostfxr test.
# Intended for a Linux container that already has Rust and curl, for example:
#   docker run --rm -v "$PWD:/src:ro" -w /src rust:1.96-bookworm sh tools/kaleidoscope/tests/host-linux.sh
set -eu
DOTNET=/opt/dotnet
curl -fsSL https://dot.net/v1/dotnet-install.sh -o /tmp/dotnet-install.sh
bash /tmp/dotnet-install.sh --channel 10.0 --install-dir "$DOTNET" >/dev/null
export DOTNET_ROOT="$DOTNET" DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 PATH="$DOTNET:$PATH"
WORK=/tmp/host-fixture
rm -rf "$WORK" && mkdir -p "$WORK/app" "$WORK/hook"
cat >"$WORK/app/app.csproj" <<'XML'
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><OutputType>Exe</OutputType><TargetFramework>net10.0</TargetFramework></PropertyGroup>
</Project>
XML
cat >"$WORK/app/Program.cs" <<'CS'
return 7;
CS
cat >"$WORK/hook/hook.csproj" <<'XML'
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup><TargetFramework>net10.0</TargetFramework><AssemblyName>hook</AssemblyName></PropertyGroup>
</Project>
XML
cat >"$WORK/hook/StartupHook.cs" <<'CS'
internal class StartupHook
{
    public static void Initialize()
    {
        System.IO.File.WriteAllText(System.Environment.GetEnvironmentVariable("KALEIDOSCOPE_TEST_MARKER")!, "hook-before-main");
    }
}
CS
dotnet publish "$WORK/app/app.csproj" -c Release -o "$WORK/app/out" >/dev/null
dotnet publish "$WORK/hook/hook.csproj" -c Release -o "$WORK/hook/out" >/dev/null
export KALEIDOSCOPE_TEST_HOSTFXR="$(ls "$DOTNET"/host/fxr/*/libhostfxr.so | head -n 1)"
export KALEIDOSCOPE_TEST_APP="$WORK/app/out/app.dll"
export KALEIDOSCOPE_TEST_HOOK="$WORK/hook/out/hook.dll"
export KALEIDOSCOPE_TEST_MARKER="$WORK/marker.txt"
cargo test -p kaleidoscope --locked --test host_linux -- --include-ignored
