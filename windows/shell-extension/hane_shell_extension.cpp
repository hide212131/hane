// Windows 11 Explorer command for Hane's signed sparse package.
// The settings/CLI-owned markers in the external directory are switches;
// packaged COM cannot see the unpackaged app's HKCU registration keys.
#include <windows.h>
#include <shobjidl.h>
#include <shlwapi.h>
#include <shellapi.h>
#include <cstring>
#include <cwchar>
#include <new>
#include <string>

namespace {
constexpr CLSID kFileCommandClsid = {0x92b8fc05, 0xdc63, 0x44ed,
                                      {0x9a, 0xfc, 0xfb, 0x10, 0xa3, 0x6b, 0xee, 0x8a}};
constexpr CLSID kFolderCommandClsid = {0xa6aab9f1, 0x298b, 0x4a9b,
                                        {0x94, 0x19, 0x81, 0x93, 0x8c, 0x99, 0x35, 0x71}};
constexpr wchar_t kLabel[] = L"Haneで開く";
constexpr char kOwnerMarker[] = "92b8fc05-dc63-44ed-9afc-fb10a36bee8a\n";
constexpr wchar_t kFileMarker[] = L"Hane.ShellIntegration.enabled";
constexpr wchar_t kFolderMarker[] = L"Hane.ShellIntegration.folder.enabled";
long g_objects = 0;

bool module_directory(std::wstring& directory) {
    HMODULE module = nullptr;
    if (!GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                                | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                            reinterpret_cast<LPCWSTR>(&g_objects), &module)) {
        return false;
    }
    wchar_t path[32768] = {};
    const DWORD length = GetModuleFileNameW(module, path, 32768);
    if (length == 0 || length >= 32768) return false;
    directory.assign(path, length);
    const auto slash = directory.find_last_of(L"\\/");
    if (slash == std::wstring::npos) return false;
    directory.resize(slash + 1);
    return true;
}

bool configured_exe(bool folder, std::wstring& exe) {
    std::wstring directory;
    if (!module_directory(directory)) return false;
    const std::wstring marker = directory + (folder ? kFolderMarker : kFileMarker);
    HANDLE file = CreateFileW(marker.c_str(), GENERIC_READ,
                              FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                              nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (file == INVALID_HANDLE_VALUE) return false;
    char contents[sizeof(kOwnerMarker)] = {};
    DWORD bytes = 0;
    const bool valid = ReadFile(file, contents, sizeof(contents), &bytes, nullptr)
        && bytes == sizeof(kOwnerMarker) - 1
        && memcmp(contents, kOwnerMarker, sizeof(kOwnerMarker) - 1) == 0;
    CloseHandle(file);
    if (!valid) return false;
    exe = directory + L"hane.exe";
    const DWORD attributes = GetFileAttributesW(exe.c_str());
    if (attributes == INVALID_FILE_ATTRIBUTES || (attributes & FILE_ATTRIBUTE_DIRECTORY)) {
        return false;
    }
    return true;
}

HRESULT selected_path(IShellItemArray* items, std::wstring& path, bool& folder) {
    if (!items) return E_INVALIDARG;
    DWORD count = 0;
    HRESULT hr = items->GetCount(&count);
    if (FAILED(hr)) return hr;
    if (count != 1) return E_INVALIDARG;
    IShellItem* item = nullptr;
    hr = items->GetItemAt(0, &item);
    if (FAILED(hr)) return hr;
    SFGAOF attributes = 0;
    hr = item->GetAttributes(SFGAO_FOLDER, &attributes);
    if (SUCCEEDED(hr)) {
        folder = (attributes & SFGAO_FOLDER) != 0;
        PWSTR raw = nullptr;
        hr = item->GetDisplayName(SIGDN_FILESYSPATH, &raw);
        if (SUCCEEDED(hr)) {
            path = raw;
            CoTaskMemFree(raw);
        }
    }
    item->Release();
    return hr;
}

class HaneCommand final : public IExplorerCommand {
public:
    explicit HaneCommand(bool folder) : folder_(folder) { InterlockedIncrement(&g_objects); }
    ~HaneCommand() { InterlockedDecrement(&g_objects); }

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID id, void** object) override {
        if (!object) return E_POINTER;
        *object = nullptr;
        if (id == IID_IUnknown || id == IID_IExplorerCommand) {
            *object = static_cast<IExplorerCommand*>(this);
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return InterlockedIncrement(&refs_); }
    ULONG STDMETHODCALLTYPE Release() override {
        const ULONG left = InterlockedDecrement(&refs_);
        if (!left) delete this;
        return left;
    }
    HRESULT STDMETHODCALLTYPE GetTitle(IShellItemArray*, LPWSTR* title) override {
        if (!title) return E_POINTER;
        return SHStrDupW(kLabel, title);
    }
    HRESULT STDMETHODCALLTYPE GetIcon(IShellItemArray*, LPWSTR* icon) override {
        if (!icon) return E_POINTER;
        *icon = nullptr;
        std::wstring exe;
        if (!configured_exe(folder_, exe)) return E_NOTIMPL;
        return SHStrDupW(exe.c_str(), icon);
    }
    HRESULT STDMETHODCALLTYPE GetToolTip(IShellItemArray*, LPWSTR* tip) override {
        if (!tip) return E_POINTER;
        *tip = nullptr;
        return E_NOTIMPL;
    }
    HRESULT STDMETHODCALLTYPE GetCanonicalName(GUID* name) override {
        if (!name) return E_POINTER;
        *name = folder_ ? kFolderCommandClsid : kFileCommandClsid;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE GetState(IShellItemArray*, BOOL, EXPCMDSTATE* state) override {
        if (!state) return E_POINTER;
        std::wstring exe;
        // Packaged COM does not see the unpackaged process's HKCU registry
        // writes. Read a marker from the sparse package's external location.
        *state = configured_exe(folder_, exe) ? ECS_ENABLED : ECS_HIDDEN;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE Invoke(IShellItemArray* items, IBindCtx*) override {
        std::wstring path, exe;
        bool folder = false;
        HRESULT hr = selected_path(items, path, folder);
        if (FAILED(hr)) return hr;
        if (folder != folder_) return E_INVALIDARG;
        if (!configured_exe(folder_, exe)) return HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND);
        const std::wstring parameter = L"\"" + path + L"\"";
        HINSTANCE result = ShellExecuteW(nullptr, L"open", exe.c_str(), parameter.c_str(),
                                         nullptr, SW_SHOWNORMAL);
        return reinterpret_cast<INT_PTR>(result) > 32 ? S_OK : E_FAIL;
    }
    HRESULT STDMETHODCALLTYPE GetFlags(EXPCMDFLAGS* flags) override {
        if (!flags) return E_POINTER;
        *flags = ECF_DEFAULT;
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE EnumSubCommands(IEnumExplorerCommand** commands) override {
        if (!commands) return E_POINTER;
        *commands = nullptr;
        return E_NOTIMPL;
    }
private:
    bool folder_;
    long refs_ = 1;
};

class HaneFactory final : public IClassFactory {
public:
    explicit HaneFactory(bool folder) : folder_(folder) { InterlockedIncrement(&g_objects); }
    ~HaneFactory() { InterlockedDecrement(&g_objects); }
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID id, void** object) override {
        if (!object) return E_POINTER;
        *object = nullptr;
        if (id == IID_IUnknown || id == IID_IClassFactory) {
            *object = static_cast<IClassFactory*>(this);
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return InterlockedIncrement(&refs_); }
    ULONG STDMETHODCALLTYPE Release() override {
        const ULONG left = InterlockedDecrement(&refs_);
        if (!left) delete this;
        return left;
    }
    HRESULT STDMETHODCALLTYPE CreateInstance(IUnknown* outer, REFIID id, void** object) override {
        if (outer) return CLASS_E_NOAGGREGATION;
        auto* command = new (std::nothrow) HaneCommand(folder_);
        if (!command) return E_OUTOFMEMORY;
        const HRESULT hr = command->QueryInterface(id, object);
        command->Release();
        return hr;
    }
    HRESULT STDMETHODCALLTYPE LockServer(BOOL lock) override {
        if (lock) InterlockedIncrement(&g_objects);
        else InterlockedDecrement(&g_objects);
        return S_OK;
    }
private:
    bool folder_;
    long refs_ = 1;
};
} // namespace

extern "C" HRESULT __stdcall DllGetClassObject(REFCLSID id, REFIID interface_id, void** object) {
    if (id != kFileCommandClsid && id != kFolderCommandClsid)
        return CLASS_E_CLASSNOTAVAILABLE;
    auto* factory = new (std::nothrow) HaneFactory(id == kFolderCommandClsid);
    if (!factory) return E_OUTOFMEMORY;
    const HRESULT hr = factory->QueryInterface(interface_id, object);
    factory->Release();
    return hr;
}

extern "C" HRESULT __stdcall DllCanUnloadNow() {
    return InterlockedCompareExchange(&g_objects, 0, 0) == 0 ? S_OK : S_FALSE;
}
