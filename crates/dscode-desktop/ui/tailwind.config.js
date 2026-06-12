/** @type {import('tailwindcss').Config} */
export default {
  content: ['./index.html', './src/**/*.{js,ts,jsx,tsx}'],
  theme: {
    extend: {
      colors: {
        sidebar: '#15161a',
        main: '#1a1c21',
        card: '#1e2128',
        border: '#2a2d35',
        accent: '#9ca3af',
        accentDim: '#373a42',
        input: '#22252c',
        userBubble: '#262b35',
        toolStart: '#1a2a1e',
        toolEnd: '#1a2229',
        think: '#1a1c21',
      },
    },
  },
  plugins: [],
};
