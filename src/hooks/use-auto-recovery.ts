import { useEffect, useRef } from "react";
import { useProfiles } from "@/hooks/use-profiles";
import { useVerge } from "@/hooks/use-verge";
import { getProxyView, patchVergeConfig, updateProfile } from "@/services/cmds";

export const useAutoRecovery = () => {
  const { verge, patchVerge } = useVerge();
  const { current, profiles } = useProfiles();
  const isRecovering = useRef(false);

  useEffect(() => {
    // Only monitor if TUN mode is actively enabled
    if (!verge?.enable_tun_mode) return;

    const interval = setInterval(async () => {
      if (isRecovering.current) return;

      try {
        const view = await getProxyView();
        if (!view || !view.proxies) return;

        // Filter for real proxy nodes (exclude Direct, Reject, and Selector groups)
        const proxyNodes = Object.values(view.proxies).filter((node: any) =>
          [
            "Shadowsocks",
            "Vmess",
            "Trojan",
            "Hysteria2",
            "Vless",
            "WireGuard",
            "Tuic",
          ].includes(node.type)
        );

        if (proxyNodes.length === 0) return;

        // Check if all nodes are dead (delay === 0 or undefined)
        const allDead = proxyNodes.every((node: any) => {
          const lastDelay = node.history?.[node.history.length - 1]?.delay;
          return !lastDelay || lastDelay === 0;
        });

        if (allDead) {
          console.warn("[Auto-Recovery] All nodes failed. Starting TUN reset...");
          isRecovering.current = true;

          // 1. Temporarily disable TUN mode so the OS gets direct internet
          await patchVergeConfig({ enable_tun_mode: false });
          await new Promise((resolve) => setTimeout(resolve, 2500));

          // 2. Update the active profile/subscription
          if (current) {
            console.log(`[Auto-Recovery] Updating subscription profile: ${current}`);
            await updateProfile(current);
            await new Promise((resolve) => setTimeout(resolve, 3000));
          }

          // 3. Re-enable TUN mode with the new nodes
          console.log("[Auto-Recovery] Profile updated. Restoring TUN mode...");
          await patchVergeConfig({ enable_tun_mode: true });

          // 60-second cooldown to prevent rapid looping
          setTimeout(() => {
            isRecovering.current = false;
          }, 60000);
        }
      } catch (err) {
        console.error("[Auto-Recovery Error]", err);
        isRecovering.current = false;
      }
    }, 15000); // Check every 15 seconds

    return () => clearInterval(interval);
  }, [verge?.enable_tun_mode, current]);
};
