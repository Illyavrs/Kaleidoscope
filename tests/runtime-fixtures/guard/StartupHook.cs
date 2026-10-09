// Fixture behavior never exists in the production bridge.
using System.Reflection;
using System.Text.Json;
using Illyaverse.Transfiguration;

public static class StartupHook
{
    public static void Initialize()
    {
        if (Environment.GetEnvironmentVariable("ILLYA_FIXTURE_THROW") == "1")
            throw new InvalidOperationException("Intentional headless startup refusal");
        string? expected = Environment.GetEnvironmentVariable("ILLYA_FIXTURE_EXPECTED_ENTRY");
        if (expected != null)
        {
            Assembly entry = Assembly.GetEntryAssembly()
                ?? throw new InvalidOperationException("fixture_entry_assembly_missing");
            bool matches = ClientAssemblyPath.Matches(entry.Location, expected);
            File.WriteAllText(Environment.GetEnvironmentVariable("ILLYA_FIXTURE_ENTRY_RECORD")!, JsonSerializer.Serialize(new {
                loaded = entry.Location,
                expected,
                canonicalLoaded = ClientAssemblyPath.Canonical(entry.Location),
                canonicalExpected = ClientAssemblyPath.Canonical(expected),
                matches,
                beforeMain = !File.Exists(Environment.GetEnvironmentVariable("ILLYA_FIXTURE_MAIN_MARKER")),
            }));
            if (!matches)
                throw new InvalidOperationException("fixture_entry_location_mismatch");
        }
        File.WriteAllText(Environment.GetEnvironmentVariable("ILLYA_FIXTURE_HOOK_MARKER")!, "hook");
    }
}

// The path helper is linked unchanged from the production bridge. This fixture
// shim keeps its rejection dependency local without loading the real bridge.
namespace Illyaverse.Transfiguration
{
    internal static class SafeStartupDiagnostic
    {
        internal static Exception Rejection(string code) => new InvalidOperationException(code);
    }
}
