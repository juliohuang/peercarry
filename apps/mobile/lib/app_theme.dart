import 'package:flutter/material.dart';

abstract final class CarryColors {
  static const ink = Color(0xFF20352E);
  static const muted = Color(0xFF687970);
  static const forest = Color(0xFF176B5B);
  static const mint = Color(0xFFE7F0E9);
  static const background = Color(0xFFF5F6F2);
  static const border = Color(0xFFDFE5DF);
}

abstract final class CarryRadii {
  static const control = 8.0;
  static const card = 12.0;
  static const pill = 24.0;
}

ThemeData carryTheme() {
  final scheme = ColorScheme.fromSeed(
    seedColor: CarryColors.forest,
    primary: CarryColors.forest,
    surface: Colors.white,
    onSurface: CarryColors.ink,
    outline: CarryColors.muted,
  );
  final base = ThemeData(useMaterial3: true, colorScheme: scheme);
  return base.copyWith(
    scaffoldBackgroundColor: CarryColors.background,
    textTheme: base.textTheme.apply(
      bodyColor: CarryColors.ink,
      displayColor: CarryColors.ink,
    ),
    appBarTheme: const AppBarTheme(
      backgroundColor: CarryColors.background,
      foregroundColor: CarryColors.ink,
      surfaceTintColor: Colors.transparent,
      elevation: 0,
      centerTitle: false,
    ),
    inputDecorationTheme: InputDecorationTheme(
      filled: true,
      fillColor: CarryColors.background,
      contentPadding: const EdgeInsets.all(16),
      hintStyle: const TextStyle(color: CarryColors.muted, fontSize: 14),
      border: OutlineInputBorder(
        borderRadius: BorderRadius.circular(CarryRadii.control),
        borderSide: const BorderSide(color: CarryColors.border),
      ),
      enabledBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(CarryRadii.control),
        borderSide: const BorderSide(color: CarryColors.border),
      ),
      focusedBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(CarryRadii.control),
        borderSide: const BorderSide(color: CarryColors.forest, width: 1.5),
      ),
    ),
    filledButtonTheme: FilledButtonThemeData(
      style: FilledButton.styleFrom(
        minimumSize: const Size(48, 50),
        padding: const EdgeInsets.symmetric(horizontal: 20, vertical: 14),
        textStyle: const TextStyle(fontSize: 15, fontWeight: FontWeight.w600),
        shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(CarryRadii.control)),
      ),
    ),
    outlinedButtonTheme: OutlinedButtonThemeData(
      style: OutlinedButton.styleFrom(
        minimumSize: const Size(48, 50),
        side: const BorderSide(color: CarryColors.border),
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
        shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(CarryRadii.control)),
      ),
    ),
    textButtonTheme: TextButtonThemeData(
      style: TextButton.styleFrom(
        shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(CarryRadii.control)),
      ),
    ),
    iconButtonTheme: IconButtonThemeData(
      style: IconButton.styleFrom(
        shape: RoundedRectangleBorder(
            borderRadius: BorderRadius.circular(CarryRadii.control)),
      ),
    ),
    navigationBarTheme: NavigationBarThemeData(
      backgroundColor: Colors.white,
      indicatorColor: CarryColors.mint,
      surfaceTintColor: Colors.transparent,
      elevation: 0,
      labelTextStyle: WidgetStateProperty.resolveWith((states) => TextStyle(
            fontSize: 12,
            fontWeight: states.contains(WidgetState.selected)
                ? FontWeight.w700
                : FontWeight.w500,
            color: states.contains(WidgetState.selected)
                ? CarryColors.forest
                : CarryColors.muted,
          )),
    ),
    chipTheme: base.chipTheme.copyWith(
      selectedColor: CarryColors.mint,
      backgroundColor: Colors.white,
      side: const BorderSide(color: CarryColors.border),
      shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(CarryRadii.pill)),
      labelStyle: const TextStyle(fontSize: 13, color: CarryColors.ink),
    ),
    dividerTheme: const DividerThemeData(color: CarryColors.border),
  );
}

class CarryCard extends StatelessWidget {
  const CarryCard({
    super.key,
    required this.child,
    this.padding = const EdgeInsets.all(20),
  });
  final Widget child;
  final EdgeInsetsGeometry padding;

  @override
  Widget build(BuildContext context) => Container(
        padding: padding,
        decoration: BoxDecoration(
          color: Colors.white,
          borderRadius: BorderRadius.circular(CarryRadii.card),
          border: Border.all(color: CarryColors.border),
        ),
        child: child,
      );
}

class CarryNotice extends StatelessWidget {
  const CarryNotice({
    super.key,
    required this.text,
    this.icon = Icons.info_outline,
    this.isError = false,
  });
  final String text;
  final IconData icon;
  final bool isError;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final color = isError ? scheme.onErrorContainer : CarryColors.forest;
    return Semantics(
      liveRegion: true,
      child: Container(
        padding: const EdgeInsets.all(14),
        decoration: BoxDecoration(
          color: isError ? scheme.errorContainer : CarryColors.mint,
          borderRadius: BorderRadius.circular(CarryRadii.card),
        ),
        child: Row(crossAxisAlignment: CrossAxisAlignment.start, children: [
          Icon(icon, size: 20, color: color),
          const SizedBox(width: 10),
          Expanded(
            child: Text(text,
                style: TextStyle(color: color, fontSize: 13, height: 1.5)),
          ),
        ]),
      ),
    );
  }
}
