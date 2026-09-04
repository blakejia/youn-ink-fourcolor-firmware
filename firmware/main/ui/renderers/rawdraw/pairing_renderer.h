/**
 * @file pairing_renderer.h
 * @brief Pairing code page renderer for rawdraw mode
 *
 * Full-screen 6-digit pairing code shown while the device polls
 * pair-claim (Lifecycle PairWaitCode). Updated via SetCode().
 */

#ifndef RAWDRAW_PAIRING_RENDERER_H
#define RAWDRAW_PAIRING_RENDERER_H

#include "page_renderer.h"
#include <string>

namespace rawdraw {

class PairingRenderer : public PageRenderer {
public:
    PairingRenderer();
    ~PairingRenderer() override;

    void Init(int width, int height) override;
    void Render(uint8_t* fb, int width, int height) override;
    bool HandleInput(const ButtonEvent& event) override;

    // Update displayed code (empty string = no code yet). Triggers refresh.
    void SetCode(const std::string& code, int expires_in);

private:
    std::string code_;
    int expires_in_ = 0;
    const lv_font_t* font_ = nullptr;
    const lv_font_t* title_font_ = nullptr;
};

}  // namespace rawdraw

#endif  // RAWDRAW_PAIRING_RENDERER_H
