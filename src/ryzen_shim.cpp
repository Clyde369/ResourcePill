// Tiny C bridge to the AMD Ryzen Master SDK. Compiled against AMD's own headers so the C++
// vtable layout and the PMTable struct come from the compiler, never from guesswork.
// The SDK DLL is loaded at runtime, so ResourcePill still runs on machines without it.
#include <windows.h>
#include "IPlatform.h"
#include "IDeviceManager.h"
#include "ICPUEx.h"

typedef IPlatform& (*GetPlatformFn)();
static ICPUEx* g_cpu = nullptr;

// 0 = ready; negative = which step failed.
extern "C" int rp_sdk_open(const wchar_t* platform_dll) {
    try {
        HMODULE h = LoadLibraryExW(platform_dll, nullptr, LOAD_WITH_ALTERED_SEARCH_PATH);
        if (!h) return -1;
        auto get = (GetPlatformFn)GetProcAddress(h, "?GetPlatform@@YAAEAVIPlatform@@XZ");
        if (!get) return -2;
        IPlatform& p = get();
        if (!p.Init()) return -3;
        g_cpu = (ICPUEx*)p.GetIDeviceManager().GetDevice(dtCPU, 0);
        return g_cpu ? 0 : -4;
    } catch (...) {
        return -5;
    }
}

// Current CPU temperature (Tctl/Tdie, °C) exactly as Ryzen Master reports it.
// Returns 0 on success, otherwise the SDK's error code (or -100 on exception).
extern "C" int rp_sdk_temperature(double* out) {
    if (!g_cpu) return -1;
    try {
        PMTable t; // fresh per call, as in AMD's sample; the SDK owns its internal buffers
        int rc = g_cpu->GetPMTableData(t);
        if (rc == 0 && t.dTemperature > 0) {
            *out = t.dTemperature;
            return 0;
        }
        double legacy = 0; // older processors still use the direct call
        int rc2 = g_cpu->GetCurrentTemperature(legacy);
        if (rc2 == 0 && legacy > 0) {
            *out = legacy;
            return 0;
        }
        return rc != 0 ? rc : rc2;
    } catch (...) {
        return -100;
    }
}
