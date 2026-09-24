// Package-independent COM smoke test for the Explorer extension DLL.
#include <windows.h>
#include <shobjidl.h>
#include <cstdio>
#include <cwchar>

constexpr CLSID kCommandClsid = {0x92b8fc05, 0xdc63, 0x44ed,
                                  {0x9a, 0xfc, 0xfb, 0x10, 0xa3, 0x6b, 0xee, 0x8a}};
constexpr CLSID kFolderCommandClsid = {0xa6aab9f1, 0x298b, 0x4a9b,
                                        {0x94, 0x19, 0x81, 0x93, 0x8c, 0x99, 0x35, 0x71}};
using GetClassObject = HRESULT(__stdcall*)(REFCLSID, REFIID, void**);
using CanUnloadNow = HRESULT(__stdcall*)();

int wmain(int argc, wchar_t** argv) {
    if (argc != 2) return 1;
    if (FAILED(CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED))) return 2;
    if (wcscmp(argv[1], L"--registered") == 0
        || wcscmp(argv[1], L"--registered-folder") == 0) {
        const bool folder = wcscmp(argv[1], L"--registered-folder") == 0;
        IExplorerCommand* registered = nullptr;
        HRESULT hr = CoCreateInstance(folder ? kFolderCommandClsid : kCommandClsid,
                                      nullptr, CLSCTX_LOCAL_SERVER,
                                      IID_IExplorerCommand,
                                      reinterpret_cast<void**>(&registered));
        std::wprintf(L"CoCreateInstance=0x%08lX\n", static_cast<unsigned long>(hr));
        if (FAILED(hr)) { CoUninitialize(); return 8; }
        EXPCMDSTATE state = ECS_HIDDEN;
        hr = registered->GetState(nullptr, FALSE, &state);
        std::wprintf(L"GetState=0x%08lX state=%d\n", static_cast<unsigned long>(hr), state);
        PWSTR tooltip = nullptr;
        HRESULT tip_hr = registered->GetToolTip(nullptr, &tooltip);
        std::wprintf(L"GetToolTip=0x%08lX %ls\n", static_cast<unsigned long>(tip_hr),
                     tooltip ? tooltip : L"");
        CoTaskMemFree(tooltip);
        registered->Release();
        CoUninitialize();
        return SUCCEEDED(hr) && (folder || state == ECS_ENABLED) ? 0 : 9;
    }
    HMODULE dll = LoadLibraryW(argv[1]);
    if (!dll) return 3;
    auto get_class = reinterpret_cast<GetClassObject>(GetProcAddress(dll, "DllGetClassObject"));
    auto can_unload = reinterpret_cast<CanUnloadNow>(GetProcAddress(dll, "DllCanUnloadNow"));
    if (!get_class || !can_unload) return 4;
    IClassFactory* factory = nullptr;
    if (FAILED(get_class(kCommandClsid, IID_IClassFactory, reinterpret_cast<void**>(&factory)))) return 5;
    IExplorerCommand* command = nullptr;
    HRESULT result = factory->CreateInstance(nullptr, IID_IExplorerCommand, reinterpret_cast<void**>(&command));
    factory->Release();
    if (FAILED(result)) return 6;
    PWSTR title = nullptr;
    result = command->GetTitle(nullptr, &title);
    const bool label_ok = SUCCEEDED(result) && title && wcscmp(title, L"Haneで開く") == 0;
    CoTaskMemFree(title);
    EXPCMDSTATE state = ECS_ENABLED;
    result = command->GetState(nullptr, FALSE, &state);
    std::wprintf(L"DirectGetState=0x%08lX state=%d\n",
                 static_cast<unsigned long>(result), state);
    const bool valid_state = SUCCEEDED(result)
        && (state == ECS_ENABLED || state == ECS_HIDDEN);
    command->Release();
    const bool unloaded = can_unload() == S_OK;
    FreeLibrary(dll);
    CoUninitialize();
    return label_ok && valid_state && unloaded ? 0 : 7;
}
