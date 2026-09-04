/**
 * @file pairing_renderer.cc
 * @brief Pairing code page - big 6-digit code + instructions
 */

#include "pairing_renderer.h"
#include "rawdraw/rawdraw.h"
#include "rawdraw/layout_utils.h"
#include "rawdraw/theme.h"
#include <cstdio>

// External font references (same set as log_renderer)
extern const lv_font_t SourceHanSansSC_Regular_slim;
extern const lv_font_t SourceHanSansSC_Medium_slim;
// MeasureTextWidth comes from rawdraw/rawdraw.h (shared helper).

namespace rawdraw {

PairingRenderer::PairingRenderer()
    : font_(&SourceHanSansSC_Regular_slim)
    , title_font_(&SourceHanSansSC_Medium_slim) {}

PairingRenderer::~PairingRenderer() = default;

void PairingRenderer::Init(int width, int height) {
    width_ = width;
    height_ = height;
    needs_full_refresh_ = true;
}

void PairingRenderer::SetCode(const std::string& code, int expires_in) {
    code_ = code;
    expires_in_ = expires_in;
    needs_full_refresh_ = true;
}

static int CenterX(int width, const char* text, const lv_font_t* font) {
    int tw = MeasureTextWidth(text, font);
    int x = (width - tw) / 2;
    return x < 0 ? 0 : x;
}

void PairingRenderer::Render(uint8_t* fb, int width, int height) {
    const auto& theme = ThemeManager::Get();
    const Color fg = theme.ColorFor(ThemeToken::TextPrimary);
    const Color secondary = theme.ColorFor(ThemeToken::TextSecondary);

    // Title near top
    const char* title = "设备配对";
    DrawText(fb, width, CenterX(width, title, title_font_), 40,
             title, title_font_, fg, height);

    if (code_.empty()) {
        const char* hint = "正在获取配对码...";
        DrawText(fb, width, CenterX(width, hint, font_), 140,
                 hint, font_, secondary, height);
        return;
    }

    // Big code centered
    DrawText(fb, width, CenterX(width, code_.c_str(), title_font_), 120,
             code_.c_str(), title_font_, fg, height);

    // Instructions
    const char* line1 = "在管理界面输入此码确认";
    DrawText(fb, width, CenterX(width, line1, font_), 190,
             line1, font_, secondary, height);

    if (expires_in_ > 0) {
        char exp[64];
        snprintf(exp, sizeof(exp), "有效期 %d 秒", expires_in_);
        DrawText(fb, width, CenterX(width, exp, font_), 220,
                 exp, font_, secondary, height);
    }
}

bool PairingRenderer::HandleInput(const ButtonEvent& event) {
    (void)event;
    return false;  // let app-level handlers (BOOT/Settings) work
}

}  // namespace rawdraw
